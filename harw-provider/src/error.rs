//! Fehlertypen der Provider-Schicht.
//!
//! Folgt Note 12 §3 und Note 11 §9. `#[derive(HarwError)]` erzeugt `Display`,
//! `std::error::Error`, die `#[from]`-Konvertierungen **und** den passenden
//! `ProviderResult<T>`-Alias (weil der Enum-Name auf `Error` endet). Es wird
//! bewusst weder `anyhow` noch `thiserror` benutzt — die `Result`-Kultur
//! stammt 1:1 aus crypt_guard.

use harw_macros::HarwError;
use harw_types::{ModelName, ProviderName};

/// Zentrale Fehlerklasse der Provider-Schicht.
///
/// Der `HarwError`-Derive emittiert zusätzlich `pub type ProviderResult<T>`.
#[derive(Debug, HarwError)]
pub enum ProviderError {
    #[msg("provider '{name}' is not registered")]
    ProviderNotRegistered { name: ProviderName },

    #[msg("provider '{name}' is already registered")]
    ProviderAlreadyRegistered { name: ProviderName },

    #[msg("more than one primary provider configured")]
    DuplicatePrimaryProvider,

    #[msg("no primary provider configured")]
    NoPrimaryProvider,

    #[msg("duplicate provider name '{name}'")]
    DuplicateProviderName { name: ProviderName },

    #[msg("duplicate model '{model}' on provider '{provider}'")]
    DuplicateModelName {
        provider: ProviderName,
        model: ModelName,
    },

    #[msg("primary provider missing")]
    PrimaryProviderMissing,

    #[msg("secondary provider '{name}' missing")]
    SecondaryProviderMissing { name: ProviderName },

    #[msg("invalid provider role transition for '{name}'")]
    InvalidProviderRoleTransition { name: ProviderName },

    #[msg("invalid fallback reference from '{provider}' to '{target}'")]
    InvalidFallbackReference {
        provider: ProviderName,
        target: ProviderName,
    },

    #[msg("unknown model '{model}' for provider '{provider}'")]
    UnknownModelForProvider {
        provider: ProviderName,
        model: ModelName,
    },

    #[msg("no models configured for provider '{provider}'")]
    NoModelsConfigured { provider: ProviderName },

    #[msg("provider registry lock poisoned")]
    RegistryPoisoned,

    #[msg("all providers failed; tried {tried:?}")]
    AllProvidersFailed { tried: Vec<ProviderFailure> },

    #[msg("feature '{feature}' is disabled")]
    FeatureDisabled { feature: &'static str },

    #[msg("invalid base url for provider '{provider}'")]
    InvalidBaseUrl { provider: ProviderName },

    #[msg("invalid provider config for provider '{provider}'")]
    InvalidProviderConfig { provider: ProviderName },

    #[msg("environment variable '{0}' is unset or empty")]
    MissingEnvVar(String),

    #[msg("provider builder is missing required field: {0}")]
    MissingProviderField(&'static str),

    #[msg("model builder is missing required field: {0}")]
    MissingModelField(&'static str),
}

impl ProviderError {
    /// Konstruktor für Lock-Poisoning (Note 11 §9: `registry_poisoned(e)`).
    ///
    /// Schluckt den konkreten `PoisonError` bewusst — er trägt nur den
    /// vergifteten Guard und ist nicht weiter verwertbar; das `tracing`-Logging
    /// passiert an der Aufrufstelle.
    #[must_use]
    pub fn registry_poisoned<E>(_err: E) -> Self {
        Self::RegistryPoisoned
    }
}

/// Ein einzelner Fehlversuch innerhalb einer Fallback-Kette.
///
/// Note 11 §8: die Fehlerkette ist Observability-Futter, kein Müll — jeder
/// Eintrag trägt *welcher* Provider *warum* gescheitert ist.
#[derive(Debug)]
pub struct ProviderFailure {
    pub provider: ProviderName,
    pub error: Box<ProviderError>,
}

impl ProviderFailure {
    #[must_use]
    pub fn new(provider: ProviderName, error: ProviderError) -> Self {
        Self {
            provider,
            error: Box::new(error),
        }
    }
}
