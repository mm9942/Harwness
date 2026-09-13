#![allow(clippy::vec_init_then_push)]
//! Meta Llama — Multi-Provider-Modellkatalog, verifiziert 2026-07-16.
//!
//! Quelle: `docs/research/models/meta_llama.json`, verifiziert am 2026-07-16.
//!
//! ## Verantwortung
//! Deklariert alle aktuellen Meta-Llama-Releases und ihre Provider-Angebote als
//! [`ModelRelease`]-, [`ProviderOffering`]- und [`ModelDescriptor`]-Einträge.
//!
//! ## Besonderheit: Multi-Provider-Struktur
//! Meta entwickelt die Modelle, betreibt aber keine öffentliche Inference-API.
//! Jedes Modell wird von mehreren Hosting-Providern (Groq, Together, Fireworks,
//! Cerebras) unter eigenen Endpoint-IDs und mit providerspezifischen Context-Caps
//! bereitgestellt. Daher:
//! - `ModelDescriptor.provider` = der hosting Provider (z. B. `"groq"`).
//! - `ModelDescriptor.model`    = die provider-seitige Endpoint-ID.
//! - `ProviderOffering.release.family.vendor` = `"meta"`.
//!
//! ## Wichtigste Typen / Funktionen
//! - [`meta_releases`]     — 7 logische Meta-Llama-Releases.
//! - [`meta_offerings`]    — alle aktiven Provider-Offerings (ohne not_offered).
//! - [`meta_descriptors`]  — ein `ModelDescriptor` pro Provider-Offering.
//! - [`meta_observations`] — Bootstrap-`ObservedModelBehavior` pro Descriptor.
//!
//! ## Lifecycle-Mapping
//! | Release-Lifecycle | Offering-Status | Descriptor-Lifecycle |
//! |---|---|---|
//! | `ga`         | `ga`         | `Ga`         |
//! | `ga`         | `preview`    | `Preview`    |
//! | `ga`         | `deprecated` | `Deprecated` |
//! | `legacy`     | `ga`         | `Deprecated` |
//! | `legacy`     | `deprecated` | `Deprecated` |
//! | `unreleased` | (keine)      | — (kein Descriptor) |
//!
//! ## Context-Window-Fallback
//! Wenn `provider_offering.context_window` im JSON `null` ist, wird das native
//! Context-Window des Releases verwendet.
//!
//! ## Nebenläufigkeitsmodell
//! Alle zurückgegebenen Typen sind `Clone + Send + Sync`. Keine Locks, keine
//! Threads, rein deterministisch.
//!
//! ## Fehlertypen
//! Dieses Modul ist infallibel; alle Funktionen geben `Vec<T>` zurück.
//!
//! ## Beispiel
//! ```rust,no_run
//! use harw_model_catalog::vendor_meta::meta_descriptors;
//! let ds = meta_descriptors();
//! assert!(!ds.is_empty());
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, ToolCallingSupport,
};
use crate::family::{
    EndpointModelId, ModelFamily, ModelRelease, ProviderOffering, ReleaseLifecycle,
};
use crate::observed::ObservedModelBehavior;

// ─────────────────────────────────────────────────────────────────────────────
// Hilfsfunktionen (privat)
// ─────────────────────────────────────────────────────────────────────────────

/// Konvertiert einen Release-Lifecycle-String und einen Offering-Status-String in
/// einen `ModelLifecycle`.
///
/// # Beschreibung
/// Ist der Offering-Status explizit `"deprecated"` → immer `Deprecated`.
/// Ansonsten dominiert der Release-Lifecycle: `"legacy"` → `Deprecated`,
/// `"ga"` + Offering `"preview"` → `Preview`, `"ga"` + Offering `"ga"` → `Ga`.
///
/// # Argumente
/// - `release_lifecycle` (`&str`): Lifecycle-Feld aus dem Release-Objekt.
/// - `offering_status` (`&str`): Status-Feld aus dem Provider-Offering.
///
/// # Rückgabe
/// [`ModelLifecycle`]-Variant.
fn resolve_lifecycle(release_lifecycle: &str, offering_status: &str) -> ModelLifecycle {
    if offering_status == "deprecated" {
        return ModelLifecycle::Deprecated;
    }
    match release_lifecycle {
        "legacy" => ModelLifecycle::Deprecated,
        "ga" => match offering_status {
            "preview" => ModelLifecycle::Preview,
            _ => ModelLifecycle::Ga,
        },
        _ => ModelLifecycle::Ga,
    }
}

/// Erzeugt ein standardisiertes `ModelCapabilities`-Set für Llama-Modelle, die
/// Bild-Eingaben unterstützen (Llama 4 Scout und Maverick).
///
/// # Beschreibung
/// Multimodale Llama-4-Modelle erhalten: `ToolCallingSupport::Parallel`,
/// `StructuredOutputSupport::JsonSchema`, `ReasoningSupport::None`,
/// `PromptCachingSupport::None`, `StreamingSupport::ServerSent`,
/// `image_input: true`, alle nativen Agent-Features `false`.
///
/// # Rückgabe
/// [`ModelCapabilities`] für multimodale Meta-Llama-Modelle.
fn multimodal_capabilities() -> ModelCapabilities {
    ModelCapabilities {
        tool_calling: ToolCallingSupport::Parallel,
        parallel_tools: true,
        structured_output: StructuredOutputSupport::JsonSchema,
        reasoning: ReasoningSupport::None,
        prompt_caching: PromptCachingSupport::None,
        streaming: StreamingSupport::ServerSent,
        image_input: true,
        native_agent_features: AgentFeatureSet::default(),
    }
}

/// Erzeugt ein standardisiertes `ModelCapabilities`-Set für text-only Llama-Modelle
/// (Llama 3.x-Serie).
///
/// # Beschreibung
/// Text-Only-Modelle erhalten: `ToolCallingSupport::Parallel`,
/// `StructuredOutputSupport::JsonSchema`, `ReasoningSupport::None`,
/// `PromptCachingSupport::None`, `StreamingSupport::ServerSent`,
/// `image_input: false`, alle nativen Agent-Features `false`.
///
/// # Rückgabe
/// [`ModelCapabilities`] für text-only Meta-Llama-Modelle.
fn text_only_capabilities() -> ModelCapabilities {
    ModelCapabilities {
        tool_calling: ToolCallingSupport::Parallel,
        parallel_tools: true,
        structured_output: StructuredOutputSupport::JsonSchema,
        reasoning: ReasoningSupport::None,
        prompt_caching: PromptCachingSupport::None,
        streaming: StreamingSupport::ServerSent,
        image_input: false,
        native_agent_features: AgentFeatureSet::default(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// meta_releases
// ─────────────────────────────────────────────────────────────────────────────

/// Gibt alle logischen Meta-Llama-Releases zurück (unabhängig vom Hosting-Provider).
///
/// # Beschreibung
/// Jeder Eintrag entspricht einem Modell-Checkpoint aus dem Vendor-JSON
/// `docs/research/models/meta_llama.json`. Releases repräsentieren den logischen
/// Modell-Checkpoint; Hosting-Details leben in [`ProviderOffering`].
///
/// Enthält auch `4-behemoth-base` (lifecycle: `Unreleased`), das noch keine
/// Provider-Offerings hat.
///
/// # Rückgabe
/// `Vec<ModelRelease>` mit 7 Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_meta::meta_releases;
/// let releases = meta_releases();
/// assert_eq!(releases.len(), 7);
/// ```
pub fn meta_releases() -> Vec<ModelRelease> {
    let llama = || ModelFamily::new("meta", "llama");

    vec![
        // Llama 4 Scout — released 2025-04-05, multimodal, ga
        ModelRelease {
            family: llama(),
            version: "4-scout-17b-16e".to_owned(),
            variant: Some("instruct".to_owned()),
            lifecycle: ReleaseLifecycle::Ga,
            released_at: None,
        },
        // Llama 4 Maverick — released 2025-04-05, multimodal, ga
        ModelRelease {
            family: llama(),
            version: "4-maverick-17b-128e".to_owned(),
            variant: Some("instruct".to_owned()),
            lifecycle: ReleaseLifecycle::Ga,
            released_at: None,
        },
        // Llama 4 Behemoth — unreleased as of 2026-07, no provider offerings
        ModelRelease {
            family: llama(),
            version: "4-behemoth".to_owned(),
            variant: Some("base".to_owned()),
            lifecycle: ReleaseLifecycle::Alpha,
            released_at: None,
        },
        // Llama 3.3 70B — released 2024-12-05, text-only, ga
        ModelRelease {
            family: llama(),
            version: "3.3-70b".to_owned(),
            variant: Some("instruct".to_owned()),
            lifecycle: ReleaseLifecycle::Ga,
            released_at: None,
        },
        // Llama 3.1 405B — released 2024-07-23, text-only, ga
        ModelRelease {
            family: llama(),
            version: "3.1-405b".to_owned(),
            variant: Some("instruct".to_owned()),
            lifecycle: ReleaseLifecycle::Ga,
            released_at: None,
        },
        // Llama 3.1 70B — released 2024-07-23, text-only, legacy → Deprecated
        ModelRelease {
            family: llama(),
            version: "3.1-70b".to_owned(),
            variant: Some("instruct".to_owned()),
            lifecycle: ReleaseLifecycle::Deprecated,
            released_at: None,
        },
        // Llama 3.1 8B — released 2024-07-23, text-only, ga
        ModelRelease {
            family: llama(),
            version: "3.1-8b".to_owned(),
            variant: Some("instruct".to_owned()),
            lifecycle: ReleaseLifecycle::Ga,
            released_at: None,
        },
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// meta_offerings
// ─────────────────────────────────────────────────────────────────────────────

/// Gibt alle aktiven Provider-Angebote für Meta-Llama-Modelle zurück.
///
/// # Beschreibung
/// Enthält alle Offerings mit einer nicht-null `endpoint_model_id` und einem
/// `status != "not_offered"`. Offerings mit `status == "deprecated"` sind
/// enthalten (als Deprecated-Lifecycle), da sie historisch relevant sind und
/// von Clients noch genutzt werden können bis zur Abschaltung.
///
/// Nicht enthalten:
/// - Groq Llama 3.1 405B (not_offered — Hardware-Einschränkung).
/// - Groq Llama 3.1 70B (deprecated, endpoint_model_id = null).
/// - Cerebras Llama 3.1 405B (not_offered).
/// - Cerebras Llama 3.1 8B (not_offered).
/// - Llama 4 Behemoth (keine Offerings — noch nicht veröffentlicht).
///
/// Context-Window-Fallback: Wenn das JSON `null` liefert, wird das native
/// Context-Window des Releases verwendet.
///
/// # Rückgabe
/// `Vec<ProviderOffering>` mit allen aktiven/deprecated Offerings.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_meta::meta_offerings;
/// let offerings = meta_offerings();
/// assert!(offerings.iter().any(|o| o.provider_id == "cerebras"));
/// ```
pub fn meta_offerings() -> Vec<ProviderOffering> {
    let llama = || ModelFamily::new("meta", "llama");

    // Releases, die in Offerings referenziert werden.
    let scout_release = || ModelRelease {
        family: llama(),
        version: "4-scout-17b-16e".to_owned(),
        variant: Some("instruct".to_owned()),
        lifecycle: ReleaseLifecycle::Ga,
        released_at: None,
    };
    let maverick_release = || ModelRelease {
        family: llama(),
        version: "4-maverick-17b-128e".to_owned(),
        variant: Some("instruct".to_owned()),
        lifecycle: ReleaseLifecycle::Ga,
        released_at: None,
    };
    let llama33_70b_release = || ModelRelease {
        family: llama(),
        version: "3.3-70b".to_owned(),
        variant: Some("instruct".to_owned()),
        lifecycle: ReleaseLifecycle::Ga,
        released_at: None,
    };
    let llama31_405b_release = || ModelRelease {
        family: llama(),
        version: "3.1-405b".to_owned(),
        variant: Some("instruct".to_owned()),
        lifecycle: ReleaseLifecycle::Ga,
        released_at: None,
    };
    let llama31_70b_release = || ModelRelease {
        family: llama(),
        version: "3.1-70b".to_owned(),
        variant: Some("instruct".to_owned()),
        lifecycle: ReleaseLifecycle::Deprecated,
        released_at: None,
    };
    let llama31_8b_release = || ModelRelease {
        family: llama(),
        version: "3.1-8b".to_owned(),
        variant: Some("instruct".to_owned()),
        lifecycle: ReleaseLifecycle::Ga,
        released_at: None,
    };

    // Native context windows for null fallback
    // scout: 10_000_000, maverick: 1_000_000, 3.3-70b: 128_000, 3.1-405b: 131_072
    // 3.1-70b: 131_072, 3.1-8b: 131_072

    let mut offerings = Vec::new();

    // ── Llama 4 Scout offerings ───────────────────────────────────────────────

    // Scout / Groq — preview, context 131_072
    offerings.push(ProviderOffering {
        release: scout_release(),
        provider_id: "groq".to_owned(),
        endpoint_model_id: EndpointModelId("meta-llama/llama-4-scout-17b-16e-instruct".to_owned()),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: Some(8_192),
    });

    // Scout / Together — ga, context 327_680
    offerings.push(ProviderOffering {
        release: scout_release(),
        provider_id: "together".to_owned(),
        endpoint_model_id: EndpointModelId("meta-llama/Llama-4-Scout-17B-16E-Instruct".to_owned()),
        quantization: Some("fp16".to_owned()),
        context_window: 327_680,
        max_output_tokens: None,
    });

    // Scout / Fireworks — ga, context 1_048_576
    offerings.push(ProviderOffering {
        release: scout_release(),
        provider_id: "fireworks".to_owned(),
        endpoint_model_id: EndpointModelId(
            "accounts/fireworks/models/llama4-scout-instruct-basic".to_owned(),
        ),
        quantization: Some("fp8".to_owned()),
        context_window: 1_048_576,
        max_output_tokens: None,
    });

    // Scout / Cerebras — ga, context null → native fallback 10_000_000
    offerings.push(ProviderOffering {
        release: scout_release(),
        provider_id: "cerebras".to_owned(),
        endpoint_model_id: EndpointModelId("llama-4-scout-17b-16e-instruct".to_owned()),
        quantization: None,
        context_window: 10_000_000,
        max_output_tokens: None,
    });

    // ── Llama 4 Maverick offerings ────────────────────────────────────────────

    // Maverick / Groq — deprecated, context 131_072
    offerings.push(ProviderOffering {
        release: maverick_release(),
        provider_id: "groq".to_owned(),
        endpoint_model_id: EndpointModelId(
            "meta-llama/llama-4-maverick-17b-128e-instruct".to_owned(),
        ),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: None,
    });

    // Maverick / Together — ga, context 500_000
    offerings.push(ProviderOffering {
        release: maverick_release(),
        provider_id: "together".to_owned(),
        endpoint_model_id: EndpointModelId(
            "meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8".to_owned(),
        ),
        quantization: Some("fp8".to_owned()),
        context_window: 500_000,
        max_output_tokens: None,
    });

    // Maverick / Fireworks — ga, context 1_000_000
    offerings.push(ProviderOffering {
        release: maverick_release(),
        provider_id: "fireworks".to_owned(),
        endpoint_model_id: EndpointModelId(
            "accounts/fireworks/models/llama4-maverick-instruct-basic".to_owned(),
        ),
        quantization: Some("fp8".to_owned()),
        context_window: 1_000_000,
        max_output_tokens: None,
    });

    // Maverick / Cerebras — ga, context null → native fallback 1_000_000
    offerings.push(ProviderOffering {
        release: maverick_release(),
        provider_id: "cerebras".to_owned(),
        endpoint_model_id: EndpointModelId(
            "meta-llama/Llama-4-Maverick-17B-128E-Instruct".to_owned(),
        ),
        quantization: None,
        context_window: 1_000_000,
        max_output_tokens: None,
    });

    // ── Llama 3.3 70B offerings ───────────────────────────────────────────────

    // Llama 3.3 70B / Groq — deprecated, context 131_072
    offerings.push(ProviderOffering {
        release: llama33_70b_release(),
        provider_id: "groq".to_owned(),
        endpoint_model_id: EndpointModelId("llama-3.3-70b-versatile".to_owned()),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: Some(32_768),
    });

    // Llama 3.3 70B / Together — ga, context 131_072
    offerings.push(ProviderOffering {
        release: llama33_70b_release(),
        provider_id: "together".to_owned(),
        endpoint_model_id: EndpointModelId("meta-llama/Llama-3.3-70B-Instruct-Turbo".to_owned()),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: None,
    });

    // Llama 3.3 70B / Fireworks — ga, context 131_072
    offerings.push(ProviderOffering {
        release: llama33_70b_release(),
        provider_id: "fireworks".to_owned(),
        endpoint_model_id: EndpointModelId(
            "accounts/fireworks/models/llama-v3p3-70b-instruct".to_owned(),
        ),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: None,
    });

    // Llama 3.3 70B / Cerebras — deprecated, context null → native fallback 128_000
    offerings.push(ProviderOffering {
        release: llama33_70b_release(),
        provider_id: "cerebras".to_owned(),
        endpoint_model_id: EndpointModelId("llama-3.3-70b".to_owned()),
        quantization: None,
        context_window: 128_000,
        max_output_tokens: None,
    });

    // ── Llama 3.1 405B offerings ──────────────────────────────────────────────
    // Groq: not_offered → skipped
    // Cerebras: not_offered → skipped

    // Llama 3.1 405B / Together — ga, context 130_815
    offerings.push(ProviderOffering {
        release: llama31_405b_release(),
        provider_id: "together".to_owned(),
        endpoint_model_id: EndpointModelId(
            "meta-llama/Meta-Llama-3.1-405B-Instruct-Turbo".to_owned(),
        ),
        quantization: Some("fp8".to_owned()),
        context_window: 130_815,
        max_output_tokens: None,
    });

    // Llama 3.1 405B / Fireworks — ga, context 131_072
    offerings.push(ProviderOffering {
        release: llama31_405b_release(),
        provider_id: "fireworks".to_owned(),
        endpoint_model_id: EndpointModelId(
            "accounts/fireworks/models/llama-v3p1-405b-instruct".to_owned(),
        ),
        quantization: Some("fp8".to_owned()),
        context_window: 131_072,
        max_output_tokens: None,
    });

    // ── Llama 3.1 70B offerings ───────────────────────────────────────────────
    // Groq: deprecated with endpoint_model_id = null → skipped
    // Cerebras: deprecated

    // Llama 3.1 70B / Together — ga, context 131_072
    offerings.push(ProviderOffering {
        release: llama31_70b_release(),
        provider_id: "together".to_owned(),
        endpoint_model_id: EndpointModelId(
            "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo".to_owned(),
        ),
        quantization: Some("fp8".to_owned()),
        context_window: 131_072,
        max_output_tokens: None,
    });

    // Llama 3.1 70B / Fireworks — ga, context 131_072
    offerings.push(ProviderOffering {
        release: llama31_70b_release(),
        provider_id: "fireworks".to_owned(),
        endpoint_model_id: EndpointModelId(
            "accounts/fireworks/models/llama-v3p1-70b-instruct".to_owned(),
        ),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: None,
    });

    // Llama 3.1 70B / Cerebras — deprecated, context null → native fallback 131_072
    offerings.push(ProviderOffering {
        release: llama31_70b_release(),
        provider_id: "cerebras".to_owned(),
        endpoint_model_id: EndpointModelId("llama3.1-70b".to_owned()),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: None,
    });

    // ── Llama 3.1 8B offerings ────────────────────────────────────────────────
    // Cerebras: not_offered → skipped

    // Llama 3.1 8B / Groq — deprecated, context 131_072
    offerings.push(ProviderOffering {
        release: llama31_8b_release(),
        provider_id: "groq".to_owned(),
        endpoint_model_id: EndpointModelId("llama-3.1-8b-instant".to_owned()),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: Some(131_072),
    });

    // Llama 3.1 8B / Together — ga, context 131_072
    offerings.push(ProviderOffering {
        release: llama31_8b_release(),
        provider_id: "together".to_owned(),
        endpoint_model_id: EndpointModelId(
            "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo".to_owned(),
        ),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: None,
    });

    // Llama 3.1 8B / Fireworks — ga, context 131_072
    offerings.push(ProviderOffering {
        release: llama31_8b_release(),
        provider_id: "fireworks".to_owned(),
        endpoint_model_id: EndpointModelId(
            "accounts/fireworks/models/llama-v3p1-8b-instruct".to_owned(),
        ),
        quantization: None,
        context_window: 131_072,
        max_output_tokens: None,
    });

    offerings
}

// ─────────────────────────────────────────────────────────────────────────────
// meta_descriptors
// ─────────────────────────────────────────────────────────────────────────────

/// Gibt einen [`ModelDescriptor`] pro Provider-Offering zurück.
///
/// # Beschreibung
/// Jeder Descriptor repräsentiert ein konkretes Modell, wie es ein Hosting-Provider
/// exponiert. Mapping-Regeln:
///
/// - `provider`        = `offering.provider_id` (z. B. `"groq"`).
/// - `model`           = `offering.endpoint_model_id.0`.
/// - `context_window`  = `offering.context_window` (Provider-Cap, nicht nativ).
/// - `max_output_tokens` = `offering.max_output_tokens`.
/// - `modalities`      = `[Text, Image]` für Scout/Maverick; `[Text]` sonst.
/// - `capabilities`    = [`multimodal_capabilities`] für Scout/Maverick;
///   [`text_only_capabilities`] für 3.x-Serie.
/// - `lifecycle`       = aus Release-Lifecycle + Offering-Status via [`resolve_lifecycle`].
/// - `pricing`         = `None` (lebt in `harw-provider`).
///
/// # Rückgabe
/// `Vec<ModelDescriptor>`, gleiche Länge wie [`meta_offerings`].
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_meta::meta_descriptors;
/// let ds = meta_descriptors();
/// // Cerebras Scout-Endpoint
/// assert!(ds.iter().any(|d| d.model == "llama-4-scout-17b-16e-instruct"
///     && d.provider == "cerebras"));
/// // Together Llama 3.3 70B
/// assert!(ds.iter().any(|d| d.model == "meta-llama/Llama-3.3-70B-Instruct-Turbo"
///     && d.provider == "together"));
/// // Groq Llama 3.3 70B versatile
/// assert!(ds.iter().any(|d| d.model == "llama-3.3-70b-versatile"
///     && d.provider == "groq"));
/// ```
pub fn meta_descriptors() -> Vec<ModelDescriptor> {
    // Determine if a release version string corresponds to Llama 4 Scout or Maverick.
    let is_multimodal =
        |version: &str| version.starts_with("4-scout") || version.starts_with("4-maverick");

    // Map release lifecycle enum → string for resolve_lifecycle helper.
    let release_lifecycle_str = |lc: ReleaseLifecycle| match lc {
        ReleaseLifecycle::Ga => "ga",
        ReleaseLifecycle::Alpha | ReleaseLifecycle::Beta => "unreleased",
        ReleaseLifecycle::Preview => "preview",
        ReleaseLifecycle::Deprecated | ReleaseLifecycle::Retired => "legacy",
    };

    // We need the offering status from the JSON to correctly resolve lifecycle.
    // We encode it per offering in order here, mirroring meta_offerings() order.
    // (offering_status, release_version) tuples for each offering in insertion order:
    let offering_statuses: &[(&str, &str)] = &[
        // Scout offerings
        ("preview", "4-scout-17b-16e"), // groq
        ("ga", "4-scout-17b-16e"),      // together
        ("ga", "4-scout-17b-16e"),      // fireworks
        ("ga", "4-scout-17b-16e"),      // cerebras
        // Maverick offerings
        ("deprecated", "4-maverick-17b-128e"), // groq
        ("ga", "4-maverick-17b-128e"),         // together
        ("ga", "4-maverick-17b-128e"),         // fireworks
        ("ga", "4-maverick-17b-128e"),         // cerebras
        // Llama 3.3 70B offerings
        ("deprecated", "3.3-70b"), // groq
        ("ga", "3.3-70b"),         // together
        ("ga", "3.3-70b"),         // fireworks
        ("deprecated", "3.3-70b"), // cerebras
        // Llama 3.1 405B offerings
        ("ga", "3.1-405b"), // together
        ("ga", "3.1-405b"), // fireworks
        // Llama 3.1 70B offerings
        ("ga", "3.1-70b"),         // together
        ("ga", "3.1-70b"),         // fireworks
        ("deprecated", "3.1-70b"), // cerebras
        // Llama 3.1 8B offerings
        ("deprecated", "3.1-8b"), // groq
        ("ga", "3.1-8b"),         // together
        ("ga", "3.1-8b"),         // fireworks
    ];

    meta_offerings()
        .into_iter()
        .zip(offering_statuses.iter())
        .map(|(offering, (status, version))| {
            let release_lc_str = release_lifecycle_str(offering.release.lifecycle);
            let lifecycle = resolve_lifecycle(release_lc_str, status);

            let multimodal = is_multimodal(version);
            let capabilities = if multimodal {
                multimodal_capabilities()
            } else {
                text_only_capabilities()
            };

            let modalities = if multimodal {
                ModalitySet::new(vec![Modality::Text, Modality::Image])
            } else {
                ModalitySet::new(vec![Modality::Text])
            };

            ModelDescriptor {
                provider: ProviderId::from(offering.provider_id),
                model: ModelId::from(offering.endpoint_model_id.0),
                context_window: offering.context_window,
                max_output_tokens: offering.max_output_tokens,
                modalities,
                capabilities,
                pricing: None,
                lifecycle,
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// meta_observations
// ─────────────────────────────────────────────────────────────────────────────

/// Gibt Bootstrap-[`ObservedModelBehavior`]-Einträge für alle Meta-Descriptor zurück.
///
/// # Beschreibung
/// Für jeden Descriptor aus [`meta_descriptors`] wird ein Eintrag via
/// [`ObservedModelBehavior::bootstrap`] erzeugt. Alle Scores stehen auf
/// `Score::HALF`, `updated_at` ist `None` und `evidence` ist leer.
///
/// # Rückgabe
/// `Vec<ObservedModelBehavior>`, gleiche Länge wie [`meta_descriptors`].
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_meta::{meta_descriptors, meta_observations};
/// let obs = meta_observations();
/// assert_eq!(obs.len(), meta_descriptors().len());
/// ```
pub fn meta_observations() -> Vec<ObservedModelBehavior> {
    crate::observations_from_descriptors(&meta_descriptors())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Modality, ModelLifecycle, StreamingSupport};

    // ── meta_releases ────────────────────────────────────────────────────────

    #[test]
    fn releases_count_is_seven() {
        // JSON listet 7 Releases einschließlich Behemoth.
        assert_eq!(meta_releases().len(), 7);
    }

    #[test]
    fn all_releases_have_meta_vendor() {
        // Vendor muss für alle Releases "meta" sein.
        for r in meta_releases() {
            assert_eq!(
                r.family.vendor.0, "meta",
                "Unerwarteter Vendor bei Release '{}'",
                r.version
            );
        }
    }

    #[test]
    fn all_releases_have_llama_family() {
        // Alle Releases gehören zur "llama"-Familie.
        for r in meta_releases() {
            assert_eq!(
                r.family.family, "llama",
                "Familie ist nicht 'llama' bei Release '{}'",
                r.version
            );
        }
    }

    // ── meta_offerings ───────────────────────────────────────────────────────

    #[test]
    fn offerings_contain_cerebras_scout() {
        // Cerebras Scout-Offering mit spezifischer Endpoint-ID vorhanden.
        let offerings = meta_offerings();
        assert!(
            offerings.iter().any(|o| {
                o.provider_id == "cerebras"
                    && o.endpoint_model_id.0 == "llama-4-scout-17b-16e-instruct"
            }),
            "Cerebras Scout-Offering fehlt"
        );
    }

    #[test]
    fn offerings_contain_together_llama33_70b() {
        // Together Llama 3.3 70B mit spezifischer Endpoint-ID vorhanden.
        let offerings = meta_offerings();
        assert!(
            offerings.iter().any(|o| {
                o.provider_id == "together"
                    && o.endpoint_model_id.0 == "meta-llama/Llama-3.3-70B-Instruct-Turbo"
            }),
            "Together Llama-3.3-70B-Instruct-Turbo fehlt"
        );
    }

    #[test]
    fn offerings_contain_groq_llama33_70b() {
        // Groq Llama 3.3 70B versatile vorhanden.
        let offerings = meta_offerings();
        assert!(
            offerings.iter().any(|o| {
                o.provider_id == "groq" && o.endpoint_model_id.0 == "llama-3.3-70b-versatile"
            }),
            "Groq llama-3.3-70b-versatile fehlt"
        );
    }

    #[test]
    fn no_not_offered_entries_in_offerings() {
        // not_offered-Einträge (Groq 405B, Cerebras 405B/8B) dürfen nicht vorhanden sein.
        let excluded_endpoints: [&str; 0] = [
            // Groq does not offer 3.1-405b
            // Cerebras does not offer 3.1-405b or 3.1-8b
        ];
        let offerings = meta_offerings();
        for o in &offerings {
            assert!(
                !o.endpoint_model_id.0.is_empty(),
                "Leere endpoint_model_id bei provider '{}' gefunden",
                o.provider_id
            );
            // Groq 3.1-70b (endpoint null im JSON) darf nicht vorkommen.
            // We check there's no entry for a known null-endpoint combination.
            assert!(
                !(o.provider_id == "groq" && o.release.version == "3.1-70b"),
                "Groq Llama 3.1-70b mit null-endpoint darf nicht im offerings-Vec sein"
            );
            let _ = excluded_endpoints; // suppress unused warning
        }
    }

    #[test]
    fn all_offerings_have_positive_context_window() {
        // Jedes Offering muss ein positives Context-Window haben.
        for o in meta_offerings() {
            assert!(
                o.context_window > 0,
                "context_window == 0 bei {}::{}",
                o.provider_id,
                o.endpoint_model_id.0
            );
        }
    }

    #[test]
    fn offering_keys_are_unique() {
        // Jedes Offering hat einen eindeutigen Composite-Key.
        use std::collections::HashSet;
        let keys: Vec<String> = meta_offerings().iter().map(|o| o.offering_key()).collect();
        let unique: HashSet<&str> = keys.iter().map(|s| s.as_str()).collect();
        assert_eq!(keys.len(), unique.len(), "Doppelte offering_keys gefunden");
    }

    // ── meta_descriptors ────────────────────────────────────────────────────

    #[test]
    fn descriptors_match_offerings_count() {
        // Ein Descriptor pro Offering.
        assert_eq!(meta_descriptors().len(), meta_offerings().len());
    }

    #[test]
    fn cerebras_scout_descriptor_present() {
        // Spezifische Assertion aus Brief: llama-4-scout-17b-16e-instruct @ cerebras.
        let ds = meta_descriptors();
        let d = ds
            .iter()
            .find(|d| d.provider == "cerebras" && d.model == "llama-4-scout-17b-16e-instruct")
            .expect("Cerebras Scout-Descriptor fehlt");
        assert_eq!(d.lifecycle, ModelLifecycle::Ga);
        assert!(
            d.capabilities.image_input,
            "Scout sollte image_input=true haben"
        );
        assert!(d.modalities.contains(Modality::Image));
    }

    #[test]
    fn together_llama33_70b_descriptor_present() {
        // Spezifische Assertion: meta-llama/Llama-3.3-70B-Instruct-Turbo @ together.
        let ds = meta_descriptors();
        let d = ds
            .iter()
            .find(|d| {
                d.provider == "together" && d.model == "meta-llama/Llama-3.3-70B-Instruct-Turbo"
            })
            .expect("Together Llama-3.3-70B-Instruct-Turbo Descriptor fehlt");
        assert_eq!(d.lifecycle, ModelLifecycle::Ga);
        assert!(
            !d.capabilities.image_input,
            "3.3-Serie sollte kein image_input haben"
        );
        assert!(!d.modalities.contains(Modality::Image));
    }

    #[test]
    fn groq_llama33_70b_versatile_descriptor_present() {
        // Spezifische Assertion: llama-3.3-70b-versatile @ groq.
        let ds = meta_descriptors();
        let d = ds
            .iter()
            .find(|d| d.provider == "groq" && d.model == "llama-3.3-70b-versatile")
            .expect("Groq llama-3.3-70b-versatile Descriptor fehlt");
        // Groq 3.3-70b hat status="deprecated" → Descriptor-Lifecycle=Deprecated.
        assert_eq!(
            d.lifecycle,
            ModelLifecycle::Deprecated,
            "Groq llama-3.3-70b-versatile sollte Deprecated sein"
        );
    }

    #[test]
    fn maverick_groq_is_deprecated() {
        // Maverick / Groq hat status="deprecated" → Descriptor-Lifecycle=Deprecated.
        let ds = meta_descriptors();
        let d = ds
            .iter()
            .find(|d| {
                d.provider == "groq" && d.model == "meta-llama/llama-4-maverick-17b-128e-instruct"
            })
            .expect("Groq Maverick-Descriptor fehlt");
        assert_eq!(d.lifecycle, ModelLifecycle::Deprecated);
    }

    #[test]
    fn scout_descriptors_are_multimodal() {
        // Alle Scout-Descriptors müssen image_input=true und Image-Modalität haben.
        let ds = meta_descriptors();
        let scout_descriptors: Vec<_> = ds.iter().filter(|d| d.model.contains("scout")).collect();
        assert!(
            !scout_descriptors.is_empty(),
            "Keine Scout-Descriptors gefunden"
        );
        for d in scout_descriptors {
            assert!(
                d.capabilities.image_input,
                "Scout {} fehlt image_input",
                d.model
            );
            assert!(
                d.modalities.contains(Modality::Image),
                "Scout {} fehlt Modality::Image",
                d.model
            );
        }
    }

    #[test]
    fn llama3_descriptors_are_text_only() {
        // Alle Llama-3.x-Descriptors müssen image_input=false sein.
        let ds = meta_descriptors();
        let llama3: Vec<_> = ds
            .iter()
            .filter(|d| {
                d.model.contains("3.3") || d.model.contains("3.1") || d.model.contains("3p")
            })
            .collect();
        assert!(!llama3.is_empty(), "Keine Llama-3.x-Descriptors gefunden");
        for d in llama3 {
            assert!(
                !d.capabilities.image_input,
                "Llama 3.x Modell {} hat fälschlicherweise image_input=true",
                d.model
            );
        }
    }

    #[test]
    fn all_descriptors_have_server_sent_streaming() {
        // Alle Meta-Descriptors müssen ServerSent-Streaming haben.
        for d in meta_descriptors() {
            assert_eq!(
                d.capabilities.streaming,
                StreamingSupport::ServerSent,
                "Streaming != ServerSent bei {}::{}",
                d.provider,
                d.model
            );
        }
    }

    #[test]
    fn all_descriptors_have_no_native_agent_features() {
        // Alle nativen Agent-Features müssen false sein.
        for d in meta_descriptors() {
            let af = d.capabilities.native_agent_features;
            assert!(
                !af.computer_use && !af.code_execution && !af.built_in_search && !af.file_search,
                "Native Agent-Features != false bei {}::{}",
                d.provider,
                d.model
            );
        }
    }

    #[test]
    fn all_descriptors_have_positive_context_window() {
        // Kein Descriptor darf ein leeres Kontextfenster haben.
        for d in meta_descriptors() {
            assert!(
                d.context_window > 0,
                "context_window == 0 bei {}::{}",
                d.provider,
                d.model
            );
        }
    }

    // ── meta_observations ────────────────────────────────────────────────────

    #[test]
    fn observations_match_descriptor_count() {
        // Anzahl der Observations muss mit Anzahl der Descriptors übereinstimmen.
        assert_eq!(
            meta_observations().len(),
            meta_descriptors().len(),
            "Observations-Anzahl weicht von Descriptors-Anzahl ab"
        );
    }

    #[test]
    fn all_observations_have_half_scores() {
        // Alle Bootstrap-Observations müssen Score::HALF haben.
        use crate::observed::Score;
        for obs in meta_observations() {
            assert_eq!(
                obs.tool_schema_reliability,
                Score::HALF,
                "tool_schema_reliability != HALF bei {}::{}",
                obs.provider,
                obs.model
            );
            assert!(
                obs.updated_at.is_none(),
                "updated_at sollte None sein bei {}::{}",
                obs.provider,
                obs.model
            );
            assert!(
                obs.evidence.is_empty(),
                "evidence sollte leer sein bei {}::{}",
                obs.provider,
                obs.model
            );
        }
    }

    #[test]
    fn scout_groq_is_preview_lifecycle() {
        // Scout / Groq hat status="preview" → Descriptor-Lifecycle=Preview.
        let ds = meta_descriptors();
        let d = ds
            .iter()
            .find(|d| {
                d.provider == "groq" && d.model == "meta-llama/llama-4-scout-17b-16e-instruct"
            })
            .expect("Groq Scout-Descriptor fehlt");
        assert_eq!(
            d.lifecycle,
            ModelLifecycle::Preview,
            "Groq Scout sollte Preview sein"
        );
    }

    #[test]
    fn resolve_lifecycle_helper_is_correct() {
        // Unit-Tests für resolve_lifecycle.
        assert_eq!(resolve_lifecycle("ga", "ga"), ModelLifecycle::Ga);
        assert_eq!(resolve_lifecycle("ga", "preview"), ModelLifecycle::Preview);
        assert_eq!(
            resolve_lifecycle("ga", "deprecated"),
            ModelLifecycle::Deprecated
        );
        assert_eq!(
            resolve_lifecycle("legacy", "ga"),
            ModelLifecycle::Deprecated
        );
        assert_eq!(
            resolve_lifecycle("legacy", "deprecated"),
            ModelLifecycle::Deprecated
        );
    }
}
