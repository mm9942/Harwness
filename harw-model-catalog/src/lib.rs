//! Provider-Katalog: eingebettete Provider-Spezifikationen, lokale
//! Credential-Quellen und optionale models.dev-Anreicherung.
//!
//! Siehe `docs/design/CONTRACT-setup-install.md` für die verbindlichen
//! Signaturen. Re-Exports werden nach dem Fanout in der Integration ergänzt.

#![forbid(unsafe_code)]

pub mod context_defaults;
pub mod descriptor;
pub mod embedded;
pub mod error;
pub mod family;
pub mod models_dev;
pub mod observed;
pub mod provenance;
pub mod resolved;
pub mod router;
pub mod runtime;
pub mod seed;
pub mod sources;
pub mod spec;
#[cfg(test)]
mod test_support;
pub mod vendor_anthropic;
pub mod vendor_meta;
pub mod vendor_mistral;
pub mod vendor_moonshot;
pub mod vendor_openai;
pub mod vendor_qwen;
pub mod vendor_xai;
pub mod vendor_zai;

pub use context_defaults::program_defaults_for;
pub use descriptor::{ModelDescriptor, ModelId, ProviderId};
pub use embedded::embedded_catalog;
pub use error::{CatalogError, CatalogResult};
pub use models_dev::enrich_models;
pub use observed::{ObservedModelBehavior, observations_from_descriptors};
pub use resolved::{
    ResolvedModel, pick_role_from_ids, resolve, resolve_all_bootstrap, resolve_from,
};
pub use router::{Candidate, ModelRole, pick, rank};
pub use runtime::{ModelRuntimeProfile, RuntimeProfileValidationError, profile_for};
pub use seed::seed_profile_providers;
pub use sources::{
    CredentialSource, DetectedCredential, ExtractRule, SourceKind, detect_local_sources,
    embedded_sources,
};
pub use spec::{AuthMethod, ProviderApi, ProviderSpec};

/// States how this crate handles model runtime profiles.
///
/// The catalog validates profile data and retains it while ranking and selecting
/// models. It does not construct context, compact conversations, retry calls,
/// dispatch tools, or spawn child agents. A runtime that needs those policies
/// must apply them explicitly at its own execution boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProfileApplication {
    /// Profile data is validated and available to a caller, but not enforced.
    CatalogOnly,
}

/// The capability provided by this catalog for [`ModelRuntimeProfile`] values.
///
/// This constant deliberately does not claim that selecting a route changes
/// runtime behavior. It is a stable boundary for callers that need to
/// distinguish catalog validation from execution-time enforcement.
pub const RUNTIME_PROFILE_APPLICATION: RuntimeProfileApplication =
    RuntimeProfileApplication::CatalogOnly;

/// A model selected for a harness role, coupled to validated runtime-profile data.
///
/// The route makes the selected profile available to the caller, but does not
/// itself apply any context, retry, tool, or delegation policy. See
/// [`RUNTIME_PROFILE_APPLICATION`] for this crate's capability boundary.
#[derive(Debug, Clone, Copy)]
#[must_use = "a selected model route contains validated profile data; pass it to an enforcement boundary explicitly when runtime control is required"]
pub struct ModelRoute<'a> {
    descriptor: &'a ModelDescriptor,
    profile: &'a ModelRuntimeProfile,
}

impl<'a> ModelRoute<'a> {
    /// Creates a route only when `profile` satisfies all catalog invariants.
    ///
    /// Validation rejects incoherent profile data; it does not enforce the
    /// profile against a model invocation.
    pub fn try_new(
        descriptor: &'a ModelDescriptor,
        profile: &'a ModelRuntimeProfile,
    ) -> Result<Self, RuntimeProfileValidationError> {
        profile.validate()?;
        Ok(Self {
            descriptor,
            profile,
        })
    }

    /// Returns the descriptor selected for this route.
    #[must_use]
    pub const fn descriptor(&self) -> &'a ModelDescriptor {
        self.descriptor
    }

    /// Returns the runtime profile data validated for this selected descriptor.
    ///
    /// The returned value remains catalog data until a runtime explicitly
    /// applies it.
    #[must_use]
    pub const fn profile(&self) -> &'a ModelRuntimeProfile {
        self.profile
    }

    /// Returns the descriptor and its validated runtime profile together.
    #[must_use]
    pub const fn into_parts(self) -> (&'a ModelDescriptor, &'a ModelRuntimeProfile) {
        (self.descriptor, self.profile)
    }
}

/// Selects the highest-ranked valid route for `role`.
///
/// This is the route-oriented counterpart to [`pick`]. Unlike `pick`, it
/// returns [`ModelRoute`], making the selected [`ModelRuntimeProfile`] part of
/// the public result. This function only ranks and validates catalog data; it
/// does not configure or control a model runtime.
#[must_use]
pub fn select_route<'a>(
    role: ModelRole,
    candidates: &'a [Candidate<'a>],
) -> Option<ModelRoute<'a>> {
    let candidate = pick(role, candidates)?;
    ModelRoute::try_new(candidate.descriptor, candidate.profile).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn select_route_retains_the_selected_descriptor_and_valid_profile() -> TestResult {
        let resolved = resolve("gpt-5").ok_or(TestError::Missing("gpt-5 in bootstrap catalog"))?;
        let candidate = resolved.as_candidate();
        let candidates = [candidate];

        let route = select_route(ModelRole::FocusedCodingWorker, &candidates)
            .ok_or(TestError::Missing("valid candidate profile"))?;

        assert_eq!(route.descriptor().model, "gpt-5");
        assert!(route.profile().validate().is_ok());
        let (descriptor, profile) = route.into_parts();
        assert_eq!(descriptor.model, "gpt-5");
        assert_eq!(profile, route.profile());
        Ok(())
    }

    #[test]
    fn model_route_rejects_an_invalid_profile() -> TestResult {
        let descriptor = resolve("gpt-5")
            .ok_or(TestError::Missing("gpt-5 in bootstrap catalog"))?
            .descriptor;
        let mut profile = profile_for("gpt-5");
        profile.max_parallel_tools = 0;

        assert!(ModelRoute::try_new(&descriptor, &profile).is_err());
        Ok(())
    }

    #[test]
    fn runtime_profiles_are_explicitly_catalog_only() {
        assert_eq!(
            RUNTIME_PROFILE_APPLICATION,
            RuntimeProfileApplication::CatalogOnly
        );
    }
}
