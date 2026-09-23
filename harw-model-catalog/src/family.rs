//! Modell-Familien-Hierarchie: Vendor → Familie → Release → Provider-Angebot.
//!
//! Dieses Modul implementiert die vier-stufige Abstraktion aus dem Architektur-Review:
//! `ModelFamily → ModelRelease → ProviderOffering → EndpointModelId`.
//!
//! ## Verantwortung
//! - Trennung von Modellentwickler (Vendor) und Modellbetreiber (Provider).
//! - Darstellung von Llama (Meta) das *von* Groq/Together/Fireworks/Cerebras *gehostet* wird,
//!   ohne die Grenze zwischen Hersteller und API-Anbieter zu verwischen.
//! - Codierung von versionierten Releases mit optionaler Variante und Lifecycle.
//! - Bereitstellung eines eindeutigen Composite-Keys für jedes Provider-Angebot.
//!
//! Siehe §17 von `coding-philosophy.md`: Provider-spezifische Realität muss die
//! Abstraktion überleben.
//!
//! ## Wichtigste Typen
//! - [`ModelVendor`] — der herstellende Anbieter (z. B. `"meta"`, `"openai"`).
//! - [`ModelFamily`] — Familie eines Vendors (z. B. Llama, GPT, Claude).
//! - [`ModelRelease`] — konkretes Release mit Version, Variante und Lifecycle.
//! - [`ReleaseLifecycle`] — Alpha bis Retired.
//! - [`ProviderOffering`] — API-Endpunkt, unter dem eine Release erreichbar ist.
//! - [`EndpointModelId`] — exakte ID für API-Requests.
//! - [`ModelAlias`] — alternativer Name für eine Endpoint-ID.
//!
//! ## Nebenläufigkeitsmodell
//! Alle Typen sind `Clone + Send + Sync`-kompatibel (keine Locks, keine Threads,
//! keine Referenzen mit begrenzter Lebensdauer). Sicher für `Arc<T>`.
//!
//! ## Fehlertypen
//! Dieses Modul ist infallibel; alle Konstruktoren sind deterministisch.
//!
//! ## Beispiel
//! ```rust,no_run
//! use harw_model_catalog::family::{ModelFamily, ModelRelease, ReleaseLifecycle};
//!
//! let family = ModelFamily::new("meta", "llama");
//! let release = ModelRelease {
//!     family,
//!     version: "3.3-70b".to_owned(),
//!     variant: Some("instruct".to_owned()),
//!     lifecycle: ReleaseLifecycle::Ga,
//!     released_at: None,
//! };
//! assert_eq!(release.canonical_id(), "meta/llama/3.3-70b-instruct");
//! ```

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// ---------------------------------------------------------------------------
// Vendor
// ---------------------------------------------------------------------------

/// Der herstellende Anbieter eines Modells — nicht der API-Betreiber.
///
/// # Beschreibung
/// Kapselt den Hersteller-Bezeichner als transparent serialisierbaren String.
/// Beispiele: `"meta"`, `"openai"`, `"anthropic"`, `"zai"`, `"moonshot"`,
/// `"alibaba"`, `"mistral"`, `"xai"`, `"deepseek"`, `"google"`.
///
/// # Wichtig
/// `ModelVendor` identifiziert den *Entwickler*, nicht den API-Betreiber.
/// Llama wird von Meta entwickelt; Groq/Together/Fireworks/Cerebras sind Hosts.
///
/// # Serialisierung
/// Transparent: `"meta"` (kein Objekt-Wrapper).
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync`; keine internen Locks.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::family::ModelVendor;
/// let v = ModelVendor("meta".to_owned());
/// assert_eq!(v.0, "meta");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelVendor(pub String);

// ---------------------------------------------------------------------------
// ModelFamily
// ---------------------------------------------------------------------------

/// Eine Modell-Familie eines Vendors — z. B. Llama, GPT, Claude, GLM, Kimi, Qwen, Grok.
///
/// # Beschreibung
/// Ordnet eine Gruppe von Releases einem einzelnen Vendor zu.
/// Die Kombination `(vendor, family)` ist innerhalb eines Katalogs eindeutig
/// und dient als logischer Namespace für alle Releases dieser Familie.
///
/// # Felder
/// - `vendor` ([`ModelVendor`]): Der herstellende Anbieter.
/// - `family` (`String`): Familien-Bezeichner in Kleinbuchstaben, z. B. `"llama"`, `"gpt"`.
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync`; keine internen Locks.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::family::ModelFamily;
/// let f = ModelFamily::new("meta", "llama");
/// assert_eq!(f.vendor.0, "meta");
/// assert_eq!(f.family, "llama");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelFamily {
    /// Der herstellende Anbieter (nicht der API-Betreiber).
    pub vendor: ModelVendor,
    /// Familien-Bezeichner in Kleinbuchstaben, z. B. `"llama"`, `"gpt"`, `"claude"`.
    pub family: String,
}

impl ModelFamily {
    /// Erstellt eine neue `ModelFamily` aus Vendor- und Familien-Bezeichner.
    ///
    /// # Beschreibung
    /// Bequemer Konstruktor, der beliebige `Into<String>`-Typen akzeptiert.
    /// Siehe §17 des Coding-Philosophy-Dokuments.
    ///
    /// # Argumente
    /// - `vendor` (`impl Into<String>`): Hersteller-Bezeichner, z. B. `"meta"`.
    /// - `family` (`impl Into<String>`): Familien-Bezeichner, z. B. `"llama"`.
    ///
    /// # Rückgabe
    /// Eine neue [`ModelFamily`].
    ///
    /// # Nebenläufigkeit
    /// Thread-sicher; erzeugt nur lokale Allokierungen.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::family::ModelFamily;
    /// let f = ModelFamily::new("openai", "gpt");
    /// assert_eq!(f.family, "gpt");
    /// ```
    pub fn new(vendor: impl Into<String>, family: impl Into<String>) -> Self {
        Self {
            vendor: ModelVendor(vendor.into()),
            family: family.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// ReleaseLifecycle
// ---------------------------------------------------------------------------

/// Lebenszyklus-Status eines konkreten Modell-Releases.
///
/// # Beschreibung
/// Identisch in Semantik zum `ModelLifecycle` in `descriptor.rs`, jedoch
/// auf Release-Ebene angesiedelt (nicht auf Provider-Offering-Ebene).
///
/// # Varianten
/// - `Alpha`: Experimentell; bricht sich ändern jederzeit.
/// - `Beta`: Vorschau mit begrenzter Stabilität.
/// - `Preview`: Öffentliche Vorschau; stabil genug für Tests.
/// - `Ga`: Generally Available; Produktionsstatus.
/// - `Deprecated`: Veraltet; Nachfolger verfügbar.
/// - `Retired`: Eingestellt; nicht mehr erreichbar.
///
/// # Serialisierung
/// `snake_case`: `"alpha"`, `"beta"`, `"preview"`, `"ga"`, `"deprecated"`, `"retired"`.
///
/// # Nebenläufigkeit
/// `Copy + Send + Sync`; kein Heap-Speicher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseLifecycle {
    /// Experimenteller Status; API kann sich ohne Vorankündigung ändern.
    Alpha,
    /// Beta-Status; eingeschränkte Stabilität, noch nicht produktionsbereit.
    Beta,
    /// Öffentliche Vorschau; stabil genug für Integrationstests.
    Preview,
    /// Generally Available; stabiler Produktionsstatus.
    Ga,
    /// Veraltet; ein Nachfolger-Release ist verfügbar.
    Deprecated,
    /// Eingestellt; das Release ist nicht mehr erreichbar.
    Retired,
}

// ---------------------------------------------------------------------------
// ModelRelease
// ---------------------------------------------------------------------------

/// Ein konkretes Release innerhalb einer Modell-Familie — Version + optionale Variante.
///
/// # Beschreibung
/// Repräsentiert einen spezifischen Checkpoint einer Modell-Familie, z. B.
/// Llama 3.3-70b-instruct oder GPT-4o-turbo. Die Release ist unabhängig von
/// den Providern, die sie hosten.
///
/// # Felder
/// - `family` ([`ModelFamily`]): Die übergeordnete Familie und ihr Vendor.
/// - `version` (`String`): Versions-String, z. B. `"3.3-70b"`, `"opus-4.8"`.
/// - `variant` (`Option<String>`): Optionale Variante, z. B. `"instruct"`, `"turbo"`.
/// - `lifecycle` ([`ReleaseLifecycle`]): Aktueller Lebenszyklus-Status.
/// - `released_at` (`Option<OffsetDateTime>`): Datum der Erst-Veröffentlichung.
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync`; keine internen Locks.
///
/// # Fehlertypen
/// Infallibel; Konstruktoren sind deterministisch.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::family::{ModelFamily, ModelRelease, ReleaseLifecycle};
/// let r = ModelRelease {
///     family: ModelFamily::new("meta", "llama"),
///     version: "3.3-70b".to_owned(),
///     variant: None,
///     lifecycle: ReleaseLifecycle::Ga,
///     released_at: None,
/// };
/// assert_eq!(r.canonical_id(), "meta/llama/3.3-70b");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRelease {
    /// Die übergeordnete Modell-Familie mit Vendor-Informationen.
    pub family: ModelFamily,
    /// Versions-String, z. B. `"4.5"`, `"3.3-70b"`, `"opus-4.8"`, `"k2-turbo"`.
    pub version: String,
    /// Optionale Variante, z. B. `"instruct"`, `"turbo"`, `"coder"`.
    /// `None`, wenn keine Variante anwendbar ist.
    #[serde(default)]
    pub variant: Option<String>,
    /// Lebenszyklus-Status dieses Releases.
    pub lifecycle: ReleaseLifecycle,
    /// Datum der Erst-Veröffentlichung, falls bekannt.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub released_at: Option<OffsetDateTime>,
}

impl ModelRelease {
    /// Berechnet die kanonische ID des Releases im Format `vendor/family/version[-variant]`.
    ///
    /// # Beschreibung
    /// Erzeugt eine eindeutige, menschenlesbare ID, die Vendor, Familie, Version
    /// und — falls vorhanden — Variante kombiniert. Die ID ist stabil solange
    /// diese Felder unverändert bleiben.
    ///
    /// # Rückgabe
    /// `String` im Format:
    /// - Ohne Variante: `"meta/llama/3.3-70b"`
    /// - Mit Variante: `"meta/llama/3.3-70b-instruct"`
    ///
    /// # Nebenläufigkeit
    /// Thread-sicher; erzeugt nur lokale Allokierungen, keine Locks.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::family::{ModelFamily, ModelRelease, ReleaseLifecycle};
    /// let r = ModelRelease {
    ///     family: ModelFamily::new("meta", "llama"),
    ///     version: "3.3-70b".to_owned(),
    ///     variant: Some("instruct".to_owned()),
    ///     lifecycle: ReleaseLifecycle::Ga,
    ///     released_at: None,
    /// };
    /// assert_eq!(r.canonical_id(), "meta/llama/3.3-70b-instruct");
    /// ```
    pub fn canonical_id(&self) -> String {
        match &self.variant {
            Some(v) => format!(
                "{}/{}/{}-{}",
                self.family.vendor.0, self.family.family, self.version, v
            ),
            None => format!(
                "{}/{}/{}",
                self.family.vendor.0, self.family.family, self.version
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// EndpointModelId
// ---------------------------------------------------------------------------

/// Die exakte String-ID, die in einer API-Anfrage an einen Provider verwendet wird.
///
/// # Beschreibung
/// Kapselt den Modell-Bezeichner, den ein Client in einem API-Request übergeben muss.
/// Dieser Bezeichner ist provider-spezifisch und kann sich von der kanonischen
/// Release-ID unterscheiden.
///
/// # Serialisierung
/// Transparent: `"llama-3.3-70b-versatile"` (kein Objekt-Wrapper).
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync`; keine internen Locks.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::family::EndpointModelId;
/// let id = EndpointModelId("gpt-5".to_owned());
/// assert_eq!(id.0, "gpt-5");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EndpointModelId(pub String);

// ---------------------------------------------------------------------------
// ProviderOffering
// ---------------------------------------------------------------------------

/// Ein Provider-Angebot: der API-Endpunkt, unter dem eine `ModelRelease` erreichbar ist.
///
/// # Beschreibung
/// Trennt klar zwischen dem logischen Release (wer das Modell entwickelte)
/// und dem operativen Hosting (wer die API betreibt). Dieselbe Release kann
/// von mehreren Providern angeboten werden — z. B. Llama 3.3-70b via Groq,
/// Together, Fireworks und Cerebras — jeweils mit eigener Endpoint-ID,
/// Quantisierung und Context-Window-Größe.
///
/// # Felder
/// - `release` ([`ModelRelease`]): Das logische Modell-Release.
/// - `provider_id` (`String`): Provider-Bezeichner im Katalog, z. B. `"groq"`.
/// - `endpoint_model_id` ([`EndpointModelId`]): Exakter Modell-Name für API-Requests.
/// - `quantization` (`Option<String>`): Optionale Quantisierungs-Info, z. B. `"fp8"`.
/// - `context_window` (`u32`): Provider-spezifisches Context-Window in Tokens.
/// - `max_output_tokens` (`Option<u32>`): Maximale Output-Tokens (falls deklariert).
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync`; keine internen Locks.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::family::{
///     ModelFamily, ModelRelease, ReleaseLifecycle, ProviderOffering, EndpointModelId,
/// };
/// let offering = ProviderOffering {
///     release: ModelRelease {
///         family: ModelFamily::new("meta", "llama"),
///         version: "3.3-70b".to_owned(),
///         variant: None,
///         lifecycle: ReleaseLifecycle::Ga,
///         released_at: None,
///     },
///     provider_id: "groq".to_owned(),
///     endpoint_model_id: EndpointModelId("llama-3.3-70b-versatile".to_owned()),
///     quantization: None,
///     context_window: 131_072,
///     max_output_tokens: Some(8_192),
/// };
/// assert!(offering.offering_key().starts_with("groq::"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderOffering {
    /// Das logische Modell-Release (Hersteller + Version + Variante).
    pub release: ModelRelease,
    /// Provider-Bezeichner im Provider-Katalog, z. B. `"groq"`, `"together"`, `"openai"`.
    pub provider_id: String,
    /// Der exakte Modell-Name, der in API-Requests verwendet werden muss.
    pub endpoint_model_id: EndpointModelId,
    /// Optionale Quantisierungs-Information, z. B. `"fp8"`, `"int4"`.
    #[serde(default)]
    pub quantization: Option<String>,
    /// Provider-spezifisches Context-Window in Tokens (kann kleiner als das native sein).
    pub context_window: u32,
    /// Maximale Output-Tokens pro Request laut Provider-Deklaration.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

impl ProviderOffering {
    /// Erzeugt einen kanonischen Composite-Key, der Provider und Endpoint eindeutig identifiziert.
    ///
    /// # Beschreibung
    /// Kombiniert `provider_id` und `endpoint_model_id.0` mit `::` als Trennzeichen.
    /// Der Key ist stabil und für den Einsatz als HashMap-Schlüssel oder Lookup-ID geeignet.
    ///
    /// # Rückgabe
    /// `String` im Format `"provider_id::endpoint_model_id"`,
    /// z. B. `"groq::llama-3.3-70b-versatile"`.
    ///
    /// # Nebenläufigkeit
    /// Thread-sicher; erzeugt nur lokale Allokierungen.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::family::{
    ///     ModelFamily, ModelRelease, ReleaseLifecycle, ProviderOffering, EndpointModelId,
    /// };
    /// let offering = ProviderOffering {
    ///     release: ModelRelease {
    ///         family: ModelFamily::new("meta", "llama"),
    ///         version: "3.3-70b".to_owned(),
    ///         variant: None,
    ///         lifecycle: ReleaseLifecycle::Ga,
    ///         released_at: None,
    ///     },
    ///     provider_id: "groq".to_owned(),
    ///     endpoint_model_id: EndpointModelId("llama-3.3-70b-versatile".to_owned()),
    ///     quantization: None,
    ///     context_window: 131_072,
    ///     max_output_tokens: Some(8_192),
    /// };
    /// assert_eq!(offering.offering_key(), "groq::llama-3.3-70b-versatile");
    /// ```
    pub fn offering_key(&self) -> String {
        format!("{}::{}", self.provider_id, self.endpoint_model_id.0)
    }
}

// ---------------------------------------------------------------------------
// ModelAlias
// ---------------------------------------------------------------------------

/// Ein Alias: mehrere Endpoint-IDs, die auf dieselbe logische Release verweisen.
///
/// # Beschreibung
/// Provider verwenden häufig mutablebare Alias-Namen (z. B. `"llama-3-latest"`),
/// die intern auf eine konkrete `EndpointModelId` aufgelöst werden.
/// `ModelAlias` modelliert diese Beziehung explizit.
///
/// # Felder
/// - `alias` (`String`): Der Alias-Name, z. B. `"llama-3-latest"`.
/// - `resolved_to` ([`EndpointModelId`]): Die kanonische Endpoint-ID.
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync`; keine internen Locks.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::family::{EndpointModelId, ModelAlias};
/// let alias = ModelAlias {
///     alias: "llama-3-latest".to_owned(),
///     resolved_to: EndpointModelId("llama-3.3-70b-versatile".to_owned()),
/// };
/// assert_eq!(alias.resolved_to.0, "llama-3.3-70b-versatile");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelAlias {
    /// Der Alias-Name, der von Clients verwendet werden kann.
    pub alias: String,
    /// Die kanonische `EndpointModelId`, auf die dieser Alias verweist.
    pub resolved_to: EndpointModelId,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    // Hilfsfunktion: erzeugt eine vollständige ModelRelease für Llama 3.3-70b.
    fn make_llama_release(variant: Option<&str>) -> ModelRelease {
        ModelRelease {
            family: ModelFamily::new("meta", "llama"),
            version: "3.3-70b".to_owned(),
            variant: variant.map(str::to_owned),
            lifecycle: ReleaseLifecycle::Ga,
            released_at: None,
        }
    }

    // Hilfsfunktion: erzeugt ein ProviderOffering für eine gegebene Release + Provider.
    fn make_offering(release: ModelRelease, provider: &str, endpoint: &str) -> ProviderOffering {
        ProviderOffering {
            release,
            provider_id: provider.to_owned(),
            endpoint_model_id: EndpointModelId(endpoint.to_owned()),
            quantization: None,
            context_window: 131_072,
            max_output_tokens: Some(8_192),
        }
    }

    #[test]
    fn family_new_builds_pair() {
        // Test 1: ModelFamily::new setzt vendor und family korrekt.
        let f = ModelFamily::new("meta", "llama");
        assert_eq!(f.vendor.0, "meta");
        assert_eq!(f.family, "llama");
    }

    #[test]
    fn release_canonical_id_no_variant() {
        // Test 2: Release ohne Variante → "meta/llama/3.3-70b".
        let r = make_llama_release(None);
        assert_eq!(r.canonical_id(), "meta/llama/3.3-70b");
    }

    #[test]
    fn release_canonical_id_with_variant() {
        // Test 3: Release mit variant="instruct" → "meta/llama/3.3-70b-instruct".
        let r = make_llama_release(Some("instruct"));
        assert_eq!(r.canonical_id(), "meta/llama/3.3-70b-instruct");
    }

    #[test]
    fn offering_key_uniquifies_by_provider() {
        // Test 4: Dieselbe Release, zwei Provider → verschiedene offering_keys.
        let release_groq = make_llama_release(None);
        let release_together = make_llama_release(None);
        let groq = make_offering(release_groq, "groq", "llama-3.3-70b-versatile");
        let together = make_offering(
            release_together,
            "together",
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        );
        assert_ne!(groq.offering_key(), together.offering_key());
        assert_eq!(groq.offering_key(), "groq::llama-3.3-70b-versatile");
        assert_eq!(
            together.offering_key(),
            "together::meta-llama/Llama-3.3-70B-Instruct-Turbo"
        );
    }

    #[test]
    fn lifecycle_serde_kebab() -> TestResult {
        // Test 5: ReleaseLifecycle::Deprecated serialisiert als "deprecated".
        let json = serde_json::to_string(&ReleaseLifecycle::Deprecated)?;
        assert_eq!(json, "\"deprecated\"");

        let roundtrip: ReleaseLifecycle = serde_json::from_str(&json)?;
        assert_eq!(roundtrip, ReleaseLifecycle::Deprecated);
        Ok(())
    }

    #[test]
    fn family_hash_eq() -> TestResult {
        // Test 6: Zwei identische ModelFamilys hashen gleich → HashMap-Key verwendbar.
        use std::collections::HashMap;
        let f1 = ModelFamily::new("meta", "llama");
        let f2 = ModelFamily::new("meta", "llama");
        assert_eq!(f1, f2);
        let mut map: HashMap<ModelFamily, &str> = HashMap::new();
        map.insert(f1, "erster Eintrag");
        // f2 ist gleich f1; überschreibt den Eintrag.
        map.insert(f2, "zweiter Eintrag");
        assert_eq!(map.len(), 1);
        let value = map
            .values()
            .next()
            .ok_or(TestError::Missing("ein Eintrag in map"))?;
        assert_eq!(*value, "zweiter Eintrag");
        Ok(())
    }

    #[test]
    fn alias_serde_roundtrip() -> TestResult {
        // Test 7: ModelAlias JSON-Roundtrip.
        let alias = ModelAlias {
            alias: "llama-3-latest".to_owned(),
            resolved_to: EndpointModelId("llama-3.3-70b-versatile".to_owned()),
        };
        let json = serde_json::to_string(&alias)?;
        let restored: ModelAlias = serde_json::from_str(&json)?;
        assert_eq!(alias, restored);
        Ok(())
    }

    #[test]
    fn provider_offering_json_roundtrip() -> TestResult {
        // Test 8: Vollständiges Offering (mit quantization) JSON-Roundtrip.
        let mut offering = make_offering(
            make_llama_release(Some("instruct")),
            "groq",
            "llama-3.3-70b-versatile",
        );
        offering.quantization = Some("fp8".to_owned());
        let json = serde_json::to_string(&offering)?;
        let restored: ProviderOffering = serde_json::from_str(&json)?;
        assert_eq!(offering, restored);
        assert_eq!(restored.quantization, Some("fp8".to_owned()));
        Ok(())
    }

    #[test]
    fn serde_transparent_endpoint_id() -> TestResult {
        // Test 9: EndpointModelId("gpt-5") serialisiert als "gpt-5", nicht als Objekt.
        let id = EndpointModelId("gpt-5".to_owned());
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"gpt-5\"");
        let restored: EndpointModelId = serde_json::from_str(&json)?;
        assert_eq!(restored.0, "gpt-5");
        Ok(())
    }

    #[test]
    fn serde_optional_variant_and_release_date() -> TestResult {
        // Test 10: Deserialisierung ohne variant- und released_at-Felder klappt.
        let json = r#"{
            "family": {"vendor": "openai", "family": "gpt"},
            "version": "4o",
            "lifecycle": "ga"
        }"#;
        let release: ModelRelease = serde_json::from_str(json)?;
        assert_eq!(release.variant, None);
        assert_eq!(release.released_at, None);
        assert_eq!(release.lifecycle, ReleaseLifecycle::Ga);
        assert_eq!(release.version, "4o");
        Ok(())
    }
}
