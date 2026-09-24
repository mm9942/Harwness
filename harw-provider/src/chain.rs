//! Fallback-Kette als abgeleitete Runtime-Sicht (Note 11 §5–§7, Note 12 §15–§16).
//!
//! Kern-Einsicht: `primary` und `fallbacks` sind keine zwei Felder — es ist *ein*
//! geordneter 1D-`Vec`. `primary` ist nur ein Kosename für `chain[0]`,
//! `fallbacks` für `chain[1..]`. Agent-Overrides sind **Operationen auf dem Vec**
//! ([`VecOp`]), kein Zwei-Felder-Merge.

use harw_types::ProviderName;

use crate::error::{ProviderError, ProviderResult};
use crate::provider::ProviderRecord;
use crate::registry::ProviderRegistry;

/// Aufgelöste Ausführungskette. `primary` + `secondaries` sind nur **Sichten**
/// auf denselben geordneten Vec.
#[derive(Clone, Debug)]
pub struct ResolvedProviderChain {
    pub primary: ProviderRecord,
    pub secondaries: Vec<ProviderRecord>,
}

impl ResolvedProviderChain {
    #[must_use]
    pub fn new(primary: ProviderRecord, secondaries: Vec<ProviderRecord>) -> Self {
        Self {
            primary,
            secondaries,
        }
    }

    #[must_use]
    pub fn primary(&self) -> &ProviderRecord {
        &self.primary
    }

    #[must_use]
    pub fn secondaries(&self) -> &[ProviderRecord] {
        &self.secondaries
    }

    /// Vollständige Kette als Borrow-Sicht: `[primary, secondaries…]`.
    #[must_use]
    pub fn all(&self) -> Vec<&ProviderRecord> {
        std::iter::once(&self.primary)
            .chain(self.secondaries.iter())
            .collect()
    }

    /// Vollständige Kette als Owned-Vec.
    #[must_use]
    pub fn into_all(self) -> Vec<ProviderRecord> {
        let mut out = Vec::with_capacity(1 + self.secondaries.len());
        out.push(self.primary);
        out.extend(self.secondaries);
        out
    }
}

/// Operation auf dem 1D-Ketten-Vec (Note 11 §5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VecOp {
    /// Provider an Position 0 ziehen, Rest erbt + shiftet (mit Dedup).
    MoveToFront(ProviderName),
    /// Schwanz leeren — nur `chain[0]` bleibt.
    TruncateTail,
    /// Schwanz durch die gegebene Liste ersetzen.
    ReplaceTail(Vec<ProviderName>),
    /// Provider hinten anhängen.
    Append(ProviderName),
}

impl VecOp {
    /// Wendet die Operation auf die Namens-Kette an.
    pub fn apply(&self, chain: &mut Vec<ProviderName>) {
        match self {
            Self::MoveToFront(name) => {
                chain.retain(|n| n != name);
                chain.insert(0, name.clone());
            }
            Self::TruncateTail => {
                chain.truncate(1);
            }
            Self::ReplaceTail(tail) => {
                chain.truncate(1);
                chain.extend(tail.iter().cloned());
            }
            Self::Append(name) => {
                chain.push(name.clone());
            }
        }
    }
}

/// Dedup unter Erhalt der Reihenfolge (erstes Vorkommen gewinnt).
fn dedup_preserving_order(chain: &mut Vec<ProviderName>) {
    let mut seen = Vec::new();
    chain.retain(|name| {
        if seen.contains(name) {
            false
        } else {
            seen.push(name.clone());
            true
        }
    });
}

/// Agent-Level-Override (Note 12 §16). Drei-Zustand-Semantik:
/// nur primary gesetzt → erbt Fallbacks; `explicit_secondaries = Some([])` →
/// leert; `Some([x])` → ersetzt.
#[derive(Clone, Debug, Default)]
pub struct AgentProviderOverride {
    pub preferred_primary: Option<ProviderName>,
    pub appended_secondaries: Vec<ProviderName>,
    pub explicit_secondaries: Option<Vec<ProviderName>>,
    pub clear_secondaries: bool,
    pub selected_model: Option<harw_types::ModelName>,
}

impl AgentProviderOverride {
    #[must_use]
    pub fn preferred_primary(&self) -> Option<&ProviderName> {
        self.preferred_primary.as_ref()
    }
    #[must_use]
    pub fn appended_secondaries(&self) -> &[ProviderName] {
        &self.appended_secondaries
    }
    #[must_use]
    pub fn explicit_secondaries(&self) -> Option<&[ProviderName]> {
        self.explicit_secondaries.as_deref()
    }
    #[must_use]
    pub fn clear_secondaries(&self) -> bool {
        self.clear_secondaries
    }
    #[must_use]
    pub fn selected_model(&self) -> Option<&harw_types::ModelName> {
        self.selected_model.as_ref()
    }

    /// Übersetzt den Override in die geordnete Operationsfolge.
    #[must_use]
    fn to_ops(&self) -> Vec<VecOp> {
        let mut ops = Vec::new();
        if let Some(primary) = &self.preferred_primary {
            ops.push(VecOp::MoveToFront(primary.clone()));
        }
        if self.clear_secondaries {
            ops.push(VecOp::TruncateTail);
        } else if let Some(explicit) = &self.explicit_secondaries {
            ops.push(VecOp::ReplaceTail(explicit.clone()));
        }
        for name in &self.appended_secondaries {
            ops.push(VecOp::Append(name.clone()));
        }
        ops
    }
}

/// Löst die Ausführungskette unter Anwendung eines Agent-Overrides auf
/// (Note 12 §16). Die Namens-Kette wird über die Registry zu Records
/// re-materialisiert; jeder unbekannte Name ist ein harter Fehler.
pub fn resolve_provider_chain_with_override(
    registry: &impl ProviderRegistry,
    override_config: &AgentProviderOverride,
) -> ProviderResult<ResolvedProviderChain> {
    // Basis-Kette: [primary, secondaries…] als Namen.
    let base = registry.resolve_execution_chain()?;
    let mut names: Vec<ProviderName> = base.all().into_iter().map(|p| p.name.clone()).collect();

    for op in override_config.to_ops() {
        op.apply(&mut names);
    }
    dedup_preserving_order(&mut names);

    if names.is_empty() {
        return Err(ProviderError::NoPrimaryProvider);
    }

    // Namen -> Records über die Registry.
    let mut records = Vec::with_capacity(names.len());
    for name in &names {
        let record = registry
            .by_name(name)
            .cloned()
            .ok_or_else(|| ProviderError::ProviderNotRegistered { name: name.clone() })?;
        records.push(record);
    }

    let mut iter = records.into_iter();
    let primary = iter.next().ok_or(ProviderError::NoPrimaryProvider)?;
    let secondaries = iter.collect();
    Ok(ResolvedProviderChain::new(primary, secondaries))
}
