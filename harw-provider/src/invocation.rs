//! Invocation / Failover-Skelett (Note 12 §17, Note 11 §7).
//!
//! Die Schleife kennt den Unterschied primary/fallback gar nicht — `primary`
//! war nur `chain[0]`. Jeder Fehlversuch wird konserviert (Observability-Futter,
//! Note 11 §8) und am Ende als [`ProviderError::AllProvidersFailed`] gebündelt.
//!
//! **Abweichung vom Skelett:** die freien `invoke_*`-Funktionen nehmen einen
//! `&impl ProviderInvoker` entgegen. Ohne einen Invoker könnte die Funktion
//! nichts ausführen; das Skelett ließ den Parameter implizit. Die Signatur ist
//! damit ehrlich ausführbar statt ein nicht-aufrufbarer Stub.

use std::time::Duration;

use harw_types::{ModelName, ProviderName};

use crate::chain::ResolvedProviderChain;
use crate::error::{ProviderError, ProviderFailure, ProviderResult};
use crate::registry::ProviderRegistry;

/// Eine Inferenz-Anfrage an einen konkreten Provider.
#[derive(Clone, Debug)]
pub struct InvocationRequest {
    pub provider: ProviderName,
    pub model: Option<ModelName>,
    pub timeout: Option<Duration>,
}

impl InvocationRequest {
    #[must_use]
    pub fn new(provider: ProviderName) -> Self {
        Self {
            provider,
            model: None,
            timeout: None,
        }
    }

    #[must_use]
    pub fn with_model(mut self, model: ModelName) -> Self {
        self.model = Some(model);
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Klont die Anfrage, zielt sie aber auf einen anderen Provider — die Form,
    /// die die Failover-Schleife pro Kettenglied braucht.
    #[must_use]
    pub fn retarget(&self, provider: ProviderName) -> Self {
        Self {
            provider,
            model: self.model.clone(),
            timeout: self.timeout,
        }
    }
}

/// Antwort eines erfolgreichen Aufrufs.
#[derive(Clone, Debug)]
pub struct InvocationResponse {
    pub provider: ProviderName,
    pub model: Option<ModelName>,
}

/// Trait, den der konkrete Transport (HTTP-Crate) implementiert.
pub trait ProviderInvoker: Send + Sync + 'static {
    fn invoke(&self, request: InvocationRequest) -> ProviderResult<InvocationResponse>;
}

/// Ruft den in `request.provider` benannten Provider der Registry über den Invoker
/// auf. Ein unbekannter Provider wird abgelehnt. Der Funktionsname bleibt aus
/// Gründen der API-Kompatibilität erhalten.
pub fn invoke_primary<I: ProviderInvoker>(
    registry: &impl ProviderRegistry,
    invoker: &I,
    request: InvocationRequest,
) -> ProviderResult<InvocationResponse> {
    registry
        .by_name(&request.provider)
        .ok_or_else(|| ProviderError::ProviderNotRegistered {
            name: request.provider.clone(),
        })?;
    invoker.invoke(request)
}

/// Läuft die Fallback-Kette durch und sammelt jeden Fehlversuch (Note 11 §7).
pub fn invoke_with_failover<I: ProviderInvoker>(
    chain: &ResolvedProviderChain,
    invoker: &I,
    request: InvocationRequest,
) -> ProviderResult<InvocationResponse> {
    let mut failures = Vec::new();
    for provider in chain.all() {
        let req = request.retarget(provider.name.clone());
        match invoker.invoke(req) {
            Ok(resp) => return Ok(resp),
            Err(e) => failures.push(ProviderFailure::new(provider.name.clone(), e)),
        }
    }
    Err(ProviderError::AllProvidersFailed { tried: failures })
}
