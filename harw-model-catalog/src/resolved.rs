//! Aggregations-Schicht des Modell-Katalogs — vollständig aufgelöste Modellsicht.
//!
//! Dieses Modul implementiert §1 des Design-Dokuments `docs/design/model-catalog-v2.md`
//! (Vier-Schichten-Architektur) sowie `philosophy.md §2` (Modellheterogenität als Ressource)
//! und `philosophy.md §16 Invariante 6` (semantisch verlustfreie Aggregation).
//!
//! # Verantwortung
//! - Reine Wert-Assembly: keine Netzwerkzugriffe, keine Locks, keine Threads.
//! - Auflösung eines Modell-Namens in alle drei Layer-Sichten:
//!   Layer 2 ([`ModelDescriptor`]), Layer 3 ([`ModelRuntimeProfile`]),
//!   Layer 4 ([`ObservedModelBehavior`]).
//! - Layer 1 (Provider) ist implizit über `descriptor.provider` zugänglich;
//!   die Auflösung des zugehörigen `ProviderSpec` geschieht separat via
//!   [`crate::embedded_catalog`].
//!
//! # Exportierte Typen
//! - [`ResolvedModel`] — vollständig aggregierte Modellsicht.
//!
//! # Exportierte Funktionen
//! - [`resolve`] — löst einen Modell-Namen mit Bootstrap-Quellen auf.
//! - [`resolve_from`] — wie `resolve`, aber mit injizierter Descriptor-Menge.
//! - [`resolve_all_bootstrap`] — löst alle Bootstrap-Descriptoren auf.
//! - [`pick_role_from_ids`] — wählt das beste Modell für eine Rolle aus IDs.
//!
//! # Nebenläufigkeitsmodell
//! Reine Wert-Assembly; alle Typen sind `Send + Sync`. Keine Locks, keine
//! geteilten Zustandsvariablen, keine Threads werden gespawnt.
//!
//! # Fehlertypen
//! Dieses Modul ist infallibel. Alle Funktionen geben `Option` oder `Vec`
//! zurück; keine Funktion kann paniken (außer in Tests).
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_model_catalog::resolved::resolve;
//!
//! let model = resolve("gpt-5").expect("gpt-5 muss im Bootstrap-Katalog sein");
//! assert_eq!(model.descriptor.model, "gpt-5");
//! ```

use serde::{Deserialize, Serialize};

use crate::descriptor::{bootstrap_descriptors, ModelDescriptor};
use crate::observed::{bootstrap_observations, ObservedModelBehavior};
use crate::router::{pick, Candidate, ModelRole};
use crate::runtime::{profile_for, ModelRuntimeProfile};

// ─────────────────────────────────────────────────────────────────────────────
// ResolvedModel
// ─────────────────────────────────────────────────────────────────────────────

/// Aggregierte Sicht auf ein Modell über alle vier Katalog-Layer.
///
/// # Beschreibung
/// `ResolvedModel` fasst die drei expliziten Datenschichten zusammen:
/// - `descriptor` (Layer 2): Provider-deklarierte technische Fähigkeiten.
/// - `runtime` (Layer 3): Harness-seitige Runtime-Policies.
/// - `observed` (Layer 4): Empirisch gemessenes Modellverhalten.
///
/// Layer 1 (Provider) ist implizit über `descriptor.provider` erreichbar;
/// die Auflösung des zugehörigen `ProviderSpec` geschieht separat via
/// [`crate::embedded_catalog`].
///
/// # Nebenläufigkeit
/// Reine Wert-Assembly; `ResolvedModel` ist `Send + Sync`.
/// Alle enthaltenen Felder sind ebenfalls `Send + Sync`.
///
/// # Serialisierung
/// Vollständig serde-kompatibel: JSON-Roundtrip ist verlustfrei (§16 Inv. 6).
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::resolved::resolve;
///
/// let resolved = resolve("gpt-4o").unwrap();
/// println!("Provider: {}", resolved.descriptor.provider);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedModel {
    /// Deklarierte technische Fähigkeiten (Layer 2).
    pub descriptor: ModelDescriptor,
    /// Harness-seitige Runtime-Policies (Layer 3).
    pub runtime: ModelRuntimeProfile,
    /// Empirisch gemessenes Verhalten (Layer 4).
    pub observed: ObservedModelBehavior,
}

impl ResolvedModel {
    /// Erzeugt einen [`Candidate`]-View auf die enthaltenen Felder.
    ///
    /// # Beschreibung
    /// Baut ein `router::Candidate`-Struct aus geborgten Referenzen auf die
    /// eigenen Felder. Nützlich um `ResolvedModel` direkt an [`pick`] oder
    /// [`crate::router::rank`] zu übergeben, ohne Ownership abzugeben.
    ///
    /// # Rückgabe
    /// Ein [`Candidate`]`<'_>` mit Lifetime gebunden an `self`.
    ///
    /// # Concurrency
    /// Rein strukturell; keine Seiteneffekte. `Send + Sync`.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::resolved::resolve;
    ///
    /// let model = resolve("gpt-5").unwrap();
    /// let candidate = model.as_candidate();
    /// assert_eq!(candidate.descriptor.model, "gpt-5");
    /// ```
    pub fn as_candidate(&self) -> Candidate<'_> {
        Candidate {
            descriptor: &self.descriptor,
            profile: &self.runtime,
            observed: &self.observed,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Interne Hilfsfunktion
// ─────────────────────────────────────────────────────────────────────────────

/// Löst die Layer-3- und Layer-4-Sichten für einen gegebenen Descriptor auf.
///
/// # Beschreibung
/// Sucht in der Bootstrap-Observation-Liste nach einer Observation mit
/// passendem `(provider, model)`-Paar. Falls nicht gefunden, wird
/// `ObservedModelBehavior::bootstrap` als konservativer Fallback verwendet.
/// Das Runtime-Profil wird immer über `profile_for` bestimmt.
///
/// # Argumente
/// - `descriptor` (`&ModelDescriptor`): der aufzulösende Descriptor.
///
/// # Rückgabe
/// Ein vollständig befülltes [`ResolvedModel`].
///
/// # Concurrency
/// Rein funktional; kein geteilter Zustand.
fn resolve_descriptor(descriptor: ModelDescriptor) -> ResolvedModel {
    let runtime = profile_for(&descriptor.model);

    let observations = bootstrap_observations();
    let observed = observations
        .into_iter()
        .find(|o| o.provider == descriptor.provider && o.model == descriptor.model)
        .unwrap_or_else(|| {
            ObservedModelBehavior::bootstrap(&descriptor.provider, &descriptor.model)
        });

    ResolvedModel {
        descriptor,
        runtime,
        observed,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Öffentliche API
// ─────────────────────────────────────────────────────────────────────────────

/// Löst einen Modell-Namen in eine vollständige [`ResolvedModel`]-Sicht auf.
///
/// # Beschreibung
/// Sucht in [`bootstrap_descriptors`] einen Descriptor mit `descriptor.model == model_id`.
/// Fällt zurück auf Bootstrap-Observation und [`crate::runtime::DEFAULT_PROFILE`], wenn
/// keine kuratierten Werte existieren (via [`crate::runtime::profile_for`]).
///
/// # Argumente
/// - `model_id` (`&str`): Modell-Kennung wie in `providers.toml` verwendet, z. B. `"gpt-5"`.
///
/// # Rückgabe
/// `Some(ResolvedModel)` wenn `bootstrap_descriptors()` einen passenden Descriptor liefert,
/// `None` andernfalls.
///
/// # Concurrency
/// Rein funktional; sicher aus mehreren Threads gleichzeitig aufrufbar.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::resolved::resolve;
///
/// assert!(resolve("gpt-5").is_some());
/// assert!(resolve("nonexistent-model").is_none());
/// ```
pub fn resolve(model_id: &str) -> Option<ResolvedModel> {
    let descriptor = bootstrap_descriptors()
        .into_iter()
        .find(|d| d.model == model_id)?;
    Some(resolve_descriptor(descriptor))
}

/// Löst einen Modell-Namen innerhalb einer benutzerdefinierten Descriptor-Menge auf.
///
/// # Beschreibung
/// Prüft zuerst die vom Aufrufer injizierte Descriptor-Quelle und fällt nur
/// ohne passenden Eintrag auf den Bootstrap-Katalog zurück. Nützlich für Tests
/// oder für benutzerdefinierte Descriptor-Listen außerhalb des Bootstrap-Katalogs.
///
/// # Argumente
/// - `model_id` (`&str`): Modell-Kennung.
/// - `descriptors` (`I`): Iterator über `&'a ModelDescriptor`-Referenzen.
///
/// # Rückgabe
/// `Some(ResolvedModel)` wenn ein injizierter oder Bootstrap-Descriptor mit
/// `descriptor.model == model_id` gefunden wird, `None` andernfalls. Der
/// injizierte Descriptor wird für Ownership-Transfer geklont.
///
/// # Concurrency
/// Rein funktional; kein geteilter Zustand.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::descriptor::bootstrap_descriptors;
/// use harw_model_catalog::resolved::resolve_from;
///
/// let descs = bootstrap_descriptors();
/// let resolved = resolve_from("gpt-4o", descs.iter());
/// assert!(resolved.is_some());
/// ```
pub fn resolve_from<'a, I>(model_id: &str, descriptors: I) -> Option<ResolvedModel>
where
    I: IntoIterator<Item = &'a ModelDescriptor>,
{
    let descriptor = descriptors
        .into_iter()
        .find(|d| d.model == model_id)
        .cloned()
        .or_else(|| {
            bootstrap_descriptors()
                .into_iter()
                .find(|d| d.model == model_id)
        })?;
    Some(resolve_descriptor(descriptor))
}

/// Löst alle Bootstrap-Descriptoren zu einer `Vec<ResolvedModel>` auf.
///
/// # Beschreibung
/// Iteriert über [`bootstrap_descriptors`] und ruft für jeden Descriptor
/// intern `resolve_descriptor` auf. Alle `Some`-Werte werden gesammelt.
/// Das Ergebnis hat immer dieselbe Länge wie `bootstrap_descriptors()`.
///
/// # Rückgabe
/// `Vec<ResolvedModel>` mit einem Eintrag pro Bootstrap-Descriptor.
///
/// # Concurrency
/// Rein funktional; kein geteilter Zustand.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::resolved::resolve_all_bootstrap;
/// use harw_model_catalog::descriptor::bootstrap_descriptors;
///
/// let all = resolve_all_bootstrap();
/// assert_eq!(all.len(), bootstrap_descriptors().len());
/// ```
pub fn resolve_all_bootstrap() -> Vec<ResolvedModel> {
    bootstrap_descriptors()
        .into_iter()
        .map(resolve_descriptor)
        .collect()
}

/// Wählt das beste Modell für eine Rolle aus einer Menge auflösbarer IDs.
///
/// # Beschreibung
/// Löst jede ID aus `model_ids` via [`resolve`] auf. Unbekannte IDs (für die
/// `resolve` `None` zurückgibt) werden ignoriert. Aus den erfolgreich aufgelösten
/// Modellen wird via [`pick`] das beste für `role` gewählt.
///
/// Das Ergebnis ist deterministisch: bei gleichen Scores entscheidet die
/// alphabetische Reihenfolge der Modell-IDs (definiert durch `router::rank`).
///
/// # Argumente
/// - `role` ([`ModelRole`]): die Rolle, für die das beste Modell gewählt wird.
/// - `model_ids` (`&[&str]`): Liste von Modell-IDs; unbekannte werden ignoriert.
///
/// # Rückgabe
/// `Some(ResolvedModel)` — das beste bekannte Modell für `role`, oder `None`
/// wenn keine einzige ID auflösbar ist.
///
/// # Concurrency
/// Rein funktional; kein geteilter Zustand.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::resolved::pick_role_from_ids;
/// use harw_model_catalog::router::ModelRole;
///
/// let best = pick_role_from_ids(ModelRole::Orchestrator, &["gpt-5", "gpt-4o", "unknown"]);
/// assert!(best.is_some());
/// ```
pub fn pick_role_from_ids(role: ModelRole, model_ids: &[&str]) -> Option<ResolvedModel> {
    // Ownership der aufgelösten Modelle halten, damit Candidate-Referenzen gültig bleiben.
    let resolved_models: Vec<ResolvedModel> =
        model_ids.iter().filter_map(|id| resolve(id)).collect();

    if resolved_models.is_empty() {
        return None;
    }

    let candidates: Vec<Candidate<'_>> = resolved_models
        .iter()
        .map(ResolvedModel::as_candidate)
        .collect();

    pick(role, &candidates).map(|winner| {
        resolved_models
            .iter()
            .find(|m| m.descriptor.model == winner.descriptor.model)
            .expect("winner muss in resolved_models liegen")
            .clone()
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::bootstrap_descriptors;
    use crate::observed::Score;
    use crate::router::ModelRole;

    // Test 1: Bekanntes Modell liefert Some.
    #[test]
    fn resolve_known_model_returns_some() {
        let result = resolve("gpt-5");
        assert!(result.is_some(), "resolve(\"gpt-5\") muss Some liefern");
        let model = result.unwrap();
        assert_eq!(model.descriptor.model, "gpt-5");
        assert_eq!(model.descriptor.provider, "openai");
    }

    // Test 2: Unbekanntes Modell liefert None.
    #[test]
    fn resolve_unknown_model_returns_none() {
        assert!(
            resolve("nope").is_none(),
            "resolve(\"nope\") muss None liefern"
        );
        assert!(resolve("").is_none(), "resolve(\"\") muss None liefern");
    }

    #[test]
    fn resolve_from_prefers_configured_descriptor_over_bootstrap() {
        let mut configured = bootstrap_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.model == "gpt-5")
            .expect("gpt-5 muss im Bootstrap-Katalog sein");
        configured.provider = crate::descriptor::ProviderId::from("configured-openai");

        let resolved = resolve_from("gpt-5", std::iter::once(&configured))
            .expect("konfigurierter Descriptor muss auflösbar sein");

        assert_eq!(resolved.descriptor.provider, "configured-openai");
    }

    #[test]
    fn resolve_from_uses_bootstrap_only_when_configured_descriptor_does_not_match() {
        let bootstrap = bootstrap_descriptors();
        let configured = bootstrap
            .first()
            .expect("Bootstrap-Katalog muss mindestens einen Descriptor enthalten");
        let fallback = bootstrap
            .iter()
            .find(|descriptor| descriptor.model != configured.model)
            .expect("Bootstrap-Katalog muss unterschiedliche Descriptoren enthalten");
        assert_ne!(configured.model, fallback.model);

        let resolved = resolve_from(fallback.model.as_str(), std::iter::once(configured))
            .expect("Fallback-Descriptor muss aus dem Bootstrap-Katalog auflösbar sein");

        assert_eq!(resolved.descriptor.model, fallback.model);
        assert_eq!(resolved.descriptor.provider, fallback.provider);
    }

    #[test]
    fn resolve_from_uses_first_matching_configured_descriptor_deterministically() {
        let bootstrap = bootstrap_descriptors();
        let mut first = bootstrap
            .iter()
            .find(|descriptor| descriptor.model == "gpt-5")
            .expect("gpt-5 muss im Bootstrap-Katalog sein")
            .clone();
        let mut second = first.clone();
        first.provider = crate::descriptor::ProviderId::from("first-provider");
        second.provider = crate::descriptor::ProviderId::from("second-provider");

        let resolved = resolve_from("gpt-5", [&first, &second])
            .expect("erster konfigurierter Descriptor muss auflösbar sein");

        assert_eq!(resolved.descriptor.provider, "first-provider");
    }

    // Test 3: Fallback auf Bootstrap-Observation wenn keine kuratierten Werte vorhanden.
    // Da bootstrap_observations alle 15 Modelle abdeckt, testen wir den Fallback
    // via resolve_from mit einem synthetischen Descriptor, der kein Bootstrap-Pendant hat.
    #[test]
    fn resolve_unknown_falls_back_to_bootstrap_observed() {
        use crate::descriptor::{
            AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelLifecycle,
            PromptCachingSupport, ReasoningSupport, StreamingSupport, StructuredOutputSupport,
            ToolCallingSupport,
        };

        let synthetic = ModelDescriptor {
            provider: crate::descriptor::ProviderId::from("synthetic-provider"),
            model: crate::descriptor::ModelId::from("synthetic-model-xyz"),
            context_window: 4096,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        };

        let result = resolve_from("synthetic-model-xyz", std::iter::once(&synthetic));
        assert!(result.is_some(), "resolve_from muss Some liefern");
        let model = result.unwrap();

        // Observation muss Bootstrap-Score sein (alle HALF, updated_at None).
        assert_eq!(
            model.observed.tool_schema_reliability,
            Score::HALF,
            "tool_schema_reliability muss HALF sein"
        );
        assert_eq!(
            model.observed.long_context_retention,
            Score::HALF,
            "long_context_retention muss HALF sein"
        );
        assert_eq!(
            model.observed.delegation_discipline,
            Score::HALF,
            "delegation_discipline muss HALF sein"
        );
        assert_eq!(
            model.observed.recovery_after_tool_error,
            Score::HALF,
            "recovery_after_tool_error muss HALF sein"
        );
        assert_eq!(
            model.observed.completion_calibration,
            Score::HALF,
            "completion_calibration muss HALF sein"
        );
        assert_eq!(
            model.observed.compaction_resilience,
            Score::HALF,
            "compaction_resilience muss HALF sein"
        );
        assert!(
            model.observed.updated_at.is_none(),
            "updated_at muss None sein"
        );
        assert!(
            model.observed.evidence.is_empty(),
            "evidence muss leer sein"
        );
    }

    // Test 4: resolve_all_bootstrap liefert gleich viele Einträge wie bootstrap_descriptors.
    #[test]
    fn resolve_all_bootstrap_matches_descriptor_count() {
        let all = resolve_all_bootstrap();
        let expected = bootstrap_descriptors().len();
        assert_eq!(
            all.len(),
            expected,
            "resolve_all_bootstrap muss {} Einträge liefern",
            expected
        );
    }

    // Test 5: pick_role_from_ids ist deterministisch.
    #[test]
    fn pick_role_from_ids_deterministic() {
        let ids = ["gpt-5", "gpt-4o"];
        let first = pick_role_from_ids(ModelRole::Orchestrator, &ids);
        let second = pick_role_from_ids(ModelRole::Orchestrator, &ids);
        assert_eq!(
            first.as_ref().map(|m| &m.descriptor.model),
            second.as_ref().map(|m| &m.descriptor.model),
            "pick_role_from_ids muss deterministisch sein"
        );
    }

    // Test 6: Unbekannte IDs werden ignoriert; nur bekannte wirken.
    #[test]
    fn pick_role_ignores_unknown_ids() {
        let with_unknown = pick_role_from_ids(
            ModelRole::FocusedCodingWorker,
            &["gpt-5", "unknown-model-xyz", "nope"],
        );
        let without_unknown = pick_role_from_ids(ModelRole::FocusedCodingWorker, &["gpt-5"]);

        // Beide sollten gpt-5 wählen, da es das einzige bekannte Modell ist.
        assert!(
            with_unknown.is_some(),
            "sollte Some liefern wenn >= 1 bekanntes Modell"
        );
        assert_eq!(
            with_unknown.unwrap().descriptor.model,
            without_unknown.unwrap().descriptor.model,
            "Unbekannte IDs dürfen das Ergebnis nicht beeinflussen"
        );
    }

    // Test 7: as_candidate projiziert Referenzen korrekt.
    #[test]
    fn as_candidate_projects_refs() {
        let model = resolve("gpt-5").expect("gpt-5 muss im Bootstrap-Katalog sein");
        let candidate = model.as_candidate();
        assert_eq!(
            candidate.descriptor.model, "gpt-5",
            "as_candidate muss descriptor.model korrekt projizieren"
        );
        assert_eq!(
            candidate.descriptor.provider, "openai",
            "as_candidate muss descriptor.provider korrekt projizieren"
        );
    }

    // Test 8: Serde JSON Roundtrip eines ResolvedModel.
    #[test]
    fn resolved_serde_roundtrip() {
        let original =
            resolve("claude-opus-4-8").expect("claude-opus-4-8 muss im Bootstrap-Katalog sein");
        let json = serde_json::to_string(&original).expect("Serialisierung muss erfolgreich sein");
        let restored: ResolvedModel =
            serde_json::from_str(&json).expect("Deserialisierung muss erfolgreich sein");
        assert_eq!(
            original, restored,
            "ResolvedModel Serde-Roundtrip muss verlustfrei sein (§16 Inv. 6)"
        );
    }
}
