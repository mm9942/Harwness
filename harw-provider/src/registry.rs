//! Provider-Registry + lazy static Fassade (Note 11 §2–§4, Note 12 §9–§11).
//!
//! Vorbild crypt_guard: globaler `Lazy<RwLock<…>>` + `Result`-Fehlerkultur.
//! Der Container ist hinter dem `ProviderRegistry`-Trait austauschbar
//! (`Vec` <-> `HashMap`) — Call-Sites ändern sich dabei nicht.

use std::collections::HashMap;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use once_cell::sync::Lazy;

use harw_types::ProviderName;

use crate::chain::ResolvedProviderChain;
use crate::error::{ProviderError, ProviderResult};
use crate::marker::ProviderRoleTag;
use crate::provider::ProviderRecord;

/// Container-agnostische Registry-Schnittstelle (Note 12 §9).
pub trait ProviderRegistry: Send + Sync + 'static {
    fn register(&mut self, provider: ProviderRecord) -> ProviderResult<()>;
    fn unregister(&mut self, name: &ProviderName) -> ProviderResult<Option<ProviderRecord>>;
    fn by_name(&self, name: &ProviderName) -> Option<&ProviderRecord>;
    fn by_name_mut(&mut self, name: &ProviderName) -> Option<&mut ProviderRecord>;
    fn primary(&self) -> Option<&ProviderRecord>;
    fn secondaries(&self) -> Vec<&ProviderRecord>;
    fn all(&self) -> Vec<&ProviderRecord>;
    fn all_owned(&self) -> Vec<ProviderRecord>;
    fn validate(&self) -> ProviderResult<()>;
    fn resolve_execution_chain(&self) -> ProviderResult<ResolvedProviderChain>;
}

/// `Vec`-basiertes Registry-Backend — die v0.1-Wahl (Note 11 §3).
#[derive(Clone, Debug, Default)]
pub struct VecProviderRegistry {
    providers: Vec<ProviderRecord>,
}

/// `HashMap`-basiertes Backend für größere Registries.
#[derive(Clone, Debug, Default)]
pub struct HashMapProviderRegistry {
    providers: HashMap<ProviderName, ProviderRecord>,
}

// ===== shared helpers =====

fn count_primaries<'a, I: Iterator<Item = &'a ProviderRecord>>(iter: I) -> usize {
    iter.filter(|p| p.role == ProviderRoleTag::Primary).count()
}

fn validate_records<'a, I: Iterator<Item = &'a ProviderRecord>>(iter: I) -> ProviderResult<()> {
    let records: Vec<&ProviderRecord> = iter.collect();
    match count_primaries(records.iter().copied()) {
        0 => Err(ProviderError::NoPrimaryProvider),
        1 => Ok(()),
        _ => Err(ProviderError::DuplicatePrimaryProvider),
    }
}

// ===== Vec backend =====

impl ProviderRegistry for VecProviderRegistry {
    fn register(&mut self, provider: ProviderRecord) -> ProviderResult<()> {
        if self.providers.iter().any(|p| p.name == provider.name) {
            return Err(ProviderError::ProviderAlreadyRegistered {
                name: provider.name,
            });
        }
        self.providers.push(provider);
        Ok(())
    }

    fn unregister(&mut self, name: &ProviderName) -> ProviderResult<Option<ProviderRecord>> {
        if let Some(idx) = self.providers.iter().position(|p| &p.name == name) {
            Ok(Some(self.providers.remove(idx)))
        } else {
            Ok(None)
        }
    }

    fn by_name(&self, name: &ProviderName) -> Option<&ProviderRecord> {
        self.providers.iter().find(|p| &p.name == name)
    }

    fn by_name_mut(&mut self, name: &ProviderName) -> Option<&mut ProviderRecord> {
        self.providers.iter_mut().find(|p| &p.name == name)
    }

    fn primary(&self) -> Option<&ProviderRecord> {
        self.providers
            .iter()
            .find(|p| p.role == ProviderRoleTag::Primary)
    }

    fn secondaries(&self) -> Vec<&ProviderRecord> {
        self.providers
            .iter()
            .filter(|p| p.role == ProviderRoleTag::Secondary)
            .collect()
    }

    fn all(&self) -> Vec<&ProviderRecord> {
        self.providers.iter().collect()
    }

    fn all_owned(&self) -> Vec<ProviderRecord> {
        self.providers.clone()
    }

    fn validate(&self) -> ProviderResult<()> {
        validate_records(self.providers.iter())
    }

    fn resolve_execution_chain(&self) -> ProviderResult<ResolvedProviderChain> {
        let primary = self
            .primary()
            .cloned()
            .ok_or(ProviderError::PrimaryProviderMissing)?;
        let secondaries = self.secondaries().into_iter().cloned().collect();
        Ok(ResolvedProviderChain::new(primary, secondaries))
    }
}

// ===== HashMap backend =====

impl ProviderRegistry for HashMapProviderRegistry {
    fn register(&mut self, provider: ProviderRecord) -> ProviderResult<()> {
        if self.providers.contains_key(&provider.name) {
            return Err(ProviderError::ProviderAlreadyRegistered {
                name: provider.name,
            });
        }
        self.providers.insert(provider.name.clone(), provider);
        Ok(())
    }

    fn unregister(&mut self, name: &ProviderName) -> ProviderResult<Option<ProviderRecord>> {
        Ok(self.providers.remove(name))
    }

    fn by_name(&self, name: &ProviderName) -> Option<&ProviderRecord> {
        self.providers.get(name)
    }

    fn by_name_mut(&mut self, name: &ProviderName) -> Option<&mut ProviderRecord> {
        self.providers.get_mut(name)
    }

    fn primary(&self) -> Option<&ProviderRecord> {
        self.providers
            .values()
            .find(|p| p.role == ProviderRoleTag::Primary)
    }

    fn secondaries(&self) -> Vec<&ProviderRecord> {
        let mut secondaries: Vec<_> = self
            .providers
            .values()
            .filter(|p| p.role == ProviderRoleTag::Secondary)
            .collect();
        // HashMap iteration is intentionally nondeterministic. Failover is an
        // execution policy, so give secondary providers a stable explicit
        // order independent of registration order or hash seed.
        secondaries.sort_by(|left, right| left.name.cmp(&right.name));
        secondaries
    }

    fn all(&self) -> Vec<&ProviderRecord> {
        self.providers.values().collect()
    }

    fn all_owned(&self) -> Vec<ProviderRecord> {
        self.providers.values().cloned().collect()
    }

    fn validate(&self) -> ProviderResult<()> {
        validate_records(self.providers.values())
    }

    fn resolve_execution_chain(&self) -> ProviderResult<ResolvedProviderChain> {
        let primary = self
            .primary()
            .cloned()
            .ok_or(ProviderError::PrimaryProviderMissing)?;
        let secondaries = self.secondaries().into_iter().cloned().collect();
        Ok(ResolvedProviderChain::new(primary, secondaries))
    }
}

// ===== Global lazy static facade (Note 12 §11) =====

/// Globale Provider-Registry. Wie crypt_guards `LOGGER`, nur mit `RwLock`.
pub static PROVIDERS: Lazy<RwLock<VecProviderRegistry>> =
    Lazy::new(|| RwLock::new(VecProviderRegistry::default()));

#[must_use]
pub fn provider_registry() -> &'static RwLock<VecProviderRegistry> {
    &PROVIDERS
}

/// Read-Guard mit graceful Poison-Degradation (Note 11 §2).
pub fn provider_registry_read() -> ProviderResult<RwLockReadGuard<'static, VecProviderRegistry>> {
    PROVIDERS.read().map_err(|e| {
        tracing_error("provider registry poisoned (read)");
        ProviderError::registry_poisoned(e)
    })
}

/// Write-Guard mit graceful Poison-Degradation.
pub fn provider_registry_write() -> ProviderResult<RwLockWriteGuard<'static, VecProviderRegistry>> {
    PROVIDERS.write().map_err(|e| {
        tracing_error("provider registry poisoned (write)");
        ProviderError::registry_poisoned(e)
    })
}

/// `tracing`-freie Degradation (AGENTS verbietet `print*`; bis `tracing` als
/// Dependency feststeht, schlucken wir die Meldung bewusst — der Fehler wird
/// als `HarwError`-Variante weitergereicht und ist die Quelle der Wahrheit).
fn tracing_error(_msg: &str) {}

pub fn register_provider(provider: ProviderRecord) -> ProviderResult<()> {
    provider_registry_write()?.register(provider)
}

pub fn unregister_provider(name: &ProviderName) -> ProviderResult<Option<ProviderRecord>> {
    provider_registry_write()?.unregister(name)
}

pub fn resolve_provider(name: &ProviderName) -> ProviderResult<ProviderRecord> {
    provider_registry_read()?
        .by_name(name)
        .cloned()
        .ok_or_else(|| ProviderError::ProviderNotRegistered { name: name.clone() })
}

pub fn resolve_primary_provider() -> ProviderResult<ProviderRecord> {
    provider_registry_read()?
        .primary()
        .cloned()
        .ok_or(ProviderError::PrimaryProviderMissing)
}

pub fn resolve_secondary_providers() -> ProviderResult<Vec<ProviderRecord>> {
    Ok(provider_registry_read()?
        .secondaries()
        .into_iter()
        .cloned()
        .collect())
}

pub fn resolve_provider_chain() -> ProviderResult<ResolvedProviderChain> {
    provider_registry_read()?.resolve_execution_chain()
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use url::Url;

    use super::{HashMapProviderRegistry, ProviderRegistry};
    use crate::auth::{ApiKeyConfig, SghAuth};
    use crate::provider::{ProviderBuilder, ProviderRecord};
    use crate::test_support::TestResult;
    use harw_types::{ProviderId, ProviderName};

    fn record(name: &str, primary: bool) -> TestResult<ProviderRecord> {
        let builder = ProviderBuilder::new()
            .id(ProviderId::from(name))
            .name(ProviderName::from(name))
            .base_url(Url::parse("https://api.example.com/v1")?)
            .auth(SghAuth::ApiKey(ApiKeyConfig {
                api_key: SecretString::new("test-key".to_owned()),
            }));

        let record = if primary {
            builder.primary().build_record()?
        } else {
            builder.build_record()?
        };
        Ok(record)
    }

    fn registry_registered_in(order: &[&str]) -> TestResult<HashMapProviderRegistry> {
        let mut registry = HashMapProviderRegistry::default();
        registry.register(record("primary", true)?)?;
        for name in order {
            registry.register(record(name, false)?)?;
        }
        Ok(registry)
    }

    #[test]
    fn hashmap_failover_order_is_stable_across_registration_orders() -> TestResult {
        let first = registry_registered_in(&["zeta", "alpha", "middle"])?;
        let second = registry_registered_in(&["middle", "zeta", "alpha"])?;

        let first_order: Vec<_> = first
            .resolve_execution_chain()?
            .secondaries()
            .iter()
            .map(|provider| provider.name.as_str().to_owned())
            .collect();
        let second_order: Vec<_> = second
            .resolve_execution_chain()?
            .secondaries()
            .iter()
            .map(|provider| provider.name.as_str().to_owned())
            .collect();

        assert_eq!(first_order, ["alpha", "middle", "zeta"]);
        assert_eq!(second_order, first_order);
        Ok(())
    }
}
