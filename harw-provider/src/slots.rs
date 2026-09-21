//! Macro-Output-Targets (Note 12 §18) und bewusst leere Andock-Slots (§21).
//!
//! Die `*Definition*`-Typen sind das, worauf die späteren `provider!`/`model!`-
//! Macros expandieren. Die Slot-Typen am Ende sind **absichtlich** nur benannt:
//! Retry/Backoff, Governor/Policy, Observability/Metrics, Customer-Settings,
//! Capability-Matching — Andockpunkte für spätere Noten.

use crate::error::ProviderResult;
use crate::marker::{ModelCapabilityMarker, RoleMarker, Unregistered};
use crate::model::{ModelBuilder, ModelRecord};
use crate::provider::{ProviderBuilder, ProviderRecord};
use crate::registry::register_provider;

/// Expansion-Ziel eines `provider!`-Macros.
pub struct ProviderDefinition<RoleTag, KindTag, AuthTag> {
    pub marker_name: &'static str,
    pub builder: ProviderBuilder<RoleTag, KindTag, AuthTag, Unregistered>,
}

/// Expansion-Ziel eines `model!`-Macros.
pub struct ModelDefinitionSpec<CapabilityTag> {
    pub marker_name: &'static str,
    pub builder: ModelBuilder<CapabilityTag>,
}

/// Baut den Provider-Record aus der Definition und registriert ihn global.
pub fn install_provider_definition<RoleTag, KindTag, AuthTag>(
    definition: ProviderDefinition<RoleTag, KindTag, AuthTag>,
) -> ProviderResult<ProviderRecord>
where
    RoleTag: RoleMarker,
{
    let record = definition.builder.build_record()?;
    register_provider(record.clone())?;
    Ok(record)
}

/// Baut den Model-Record aus der Definition.
pub fn install_model_definition<CapabilityTag>(
    definition: ModelDefinitionSpec<CapabilityTag>,
) -> ProviderResult<ModelRecord>
where
    CapabilityTag: ModelCapabilityMarker,
{
    definition.builder.build_record()
}

// ===== Offene Slots für spätere Noten (Note 12 §21) =====

/// Retry / Backoff (eigene Note, sobald die Fallback-Schicht steht).
#[derive(Clone, Copy, Debug, Default)]
pub struct RetryPolicy;

/// Governor / Policy-Schicht.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProviderGovernor;

/// Auswahl-Trace für Observability.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProviderSelectionTrace;

/// Provider-Metriken.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProviderMetrics;

/// Mandanten-spezifische Provider-Policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct CustomerProviderPolicy;

/// Capability-Matching-Matrix.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProviderCapabilityMatrix;
