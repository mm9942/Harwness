//! Macro-Output-Targets (Note 12 §18) und bewusst leere Andock-Slots (§21).
//!
//! Die `*Definition*`-Typen sind das, worauf die späteren `provider!`/`model!`-
//! Macros expandieren. Die Slot-Typen am Ende (Retry/Backoff, Governor/Policy,
//! Auswahl-Trace, Metriken, Customer-Policy, Capability-Matrix) tragen eine
//! kleine, eigenständige Implementierung auf Basis der erased Records dieser
//! Crate. Die HTTP-seitigen Pendants (`harw-provider-http::retry::RetryPolicy`,
//! `ProviderRateLimiter`) können hier nicht re-exportiert werden, weil
//! `harw-provider-http` selbst von dieser Crate abhängt.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use harw_types::{CustomerId, ProviderName};

use crate::error::ProviderResult;
use crate::marker::{ModelCapabilityMarker, ModelCapabilityTag, RoleMarker, Unregistered};
use crate::model::{ModelBuilder, ModelRecord};
use crate::provider::{ProviderBuilder, ProviderRecord, ProviderSettings};
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

// ===== Andock-Slots (Note 12 §21), minimal implementiert =====

/// Retry / Backoff-Parameter auf Provider-Ebene.
///
/// # Description
/// `max_attempts` zählt **alle** Versuche inklusive des ersten (0 wird wie 1
/// behandelt). Die Wartezeit wächst exponentiell ab `base_delay` und wird bei
/// `max_delay` gedeckelt. Bewusst deterministisch (kein Jitter) — Jitter und
/// `retry-after`-Auswertung liegen in `harw-provider-http::retry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Höchstzahl der Versuche inklusive des ersten.
    pub max_attempts: u32,
    /// Wartezeit vor der ersten Wiederholung.
    pub base_delay: Duration,
    /// Obergrenze jeder einzelnen Wartezeit.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    /// 4 Versuche, 10 s Basis, 30 s Deckel (wie der HTTP-Default).
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// Leitet die Policy aus den Provider-Settings ab.
    ///
    /// # Returns
    /// `max_retries = n` ergibt `n + 1` Versuche; fehlt der Wert, gilt der Default.
    #[must_use]
    pub fn from_settings(settings: &ProviderSettings) -> Self {
        let mut policy = Self::default();
        if let Some(retries) = settings.max_retries {
            policy.max_attempts = retries.saturating_add(1);
        }
        policy
    }

    /// Effektive Versuchszahl (mindestens 1).
    #[must_use]
    pub fn effective_attempts(&self) -> u32 {
        self.max_attempts.max(1)
    }

    /// Ob nach `attempts_made` bereits erfolgten Versuchen noch einer folgen darf.
    #[must_use]
    pub fn should_retry(&self, attempts_made: u32) -> bool {
        attempts_made < self.effective_attempts()
    }

    /// Wartezeit vor Wiederholung `retry_index` (0-basiert):
    /// `min(max_delay, base_delay · 2^retry_index)`.
    #[must_use]
    pub fn backoff_delay(&self, retry_index: u32) -> Duration {
        let factor = 1_u32.checked_shl(retry_index.min(31)).unwrap_or(u32::MAX);
        self.base_delay
            .checked_mul(factor)
            .unwrap_or(self.max_delay)
            .min(self.max_delay)
    }
}

/// Governor / Policy-Schicht: entscheidet, ob ein Provider überhaupt
/// angesprochen werden darf.
///
/// # Description
/// Zugelassen ist ein Provider, wenn er nicht explizit gesperrt ist und seine
/// Settings ihn nicht mit `enabled = Some(false)` abschalten.
#[derive(Clone, Debug, Default)]
pub struct ProviderGovernor {
    denied: HashSet<ProviderName>,
}

impl ProviderGovernor {
    /// Sperrt einen Provider; liefert `true`, wenn er vorher zugelassen war.
    pub fn deny(&mut self, provider: ProviderName) -> bool {
        self.denied.insert(provider)
    }

    /// Hebt eine Sperre auf; liefert `true`, wenn eine Sperre bestand.
    pub fn allow(&mut self, provider: &ProviderName) -> bool {
        self.denied.remove(provider)
    }

    /// Ob der Provider explizit gesperrt ist.
    #[must_use]
    pub fn is_denied(&self, provider: &ProviderName) -> bool {
        self.denied.contains(provider)
    }

    /// Ob der Provider-Record angesprochen werden darf.
    #[must_use]
    pub fn admits(&self, record: &ProviderRecord) -> bool {
        record.settings.enabled != Some(false) && !self.is_denied(&record.name)
    }

    /// Filtert eine Kandidatenliste, Reihenfolge bleibt erhalten.
    #[must_use]
    pub fn filter<'a, I>(&self, records: I) -> Vec<&'a ProviderRecord>
    where
        I: IntoIterator<Item = &'a ProviderRecord>,
    {
        records
            .into_iter()
            .filter(|record| self.admits(record))
            .collect()
    }
}

/// Entscheidung zu einem Kandidaten im [`ProviderSelectionTrace`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectionDecision {
    /// Der Kandidat wurde gewählt.
    Selected,
    /// Der Kandidat wurde übersprungen (z. B. Governor, Capability).
    Skipped { reason: String },
    /// Der Kandidat wurde versucht und ist gescheitert.
    Failed { reason: String },
}

/// Ein Eintrag Kandidat → Entscheidung.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionEntry {
    pub provider: ProviderName,
    pub decision: SelectionDecision,
}

/// Auswahl-Trace für Observability: geordnete Liste Kandidat → Entscheidung.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderSelectionTrace {
    entries: Vec<SelectionEntry>,
}

impl ProviderSelectionTrace {
    /// Hängt eine Entscheidung an.
    pub fn push(&mut self, provider: ProviderName, decision: SelectionDecision) {
        self.entries.push(SelectionEntry { provider, decision });
    }

    /// Iteriert die Einträge in Aufzeichnungsreihenfolge.
    pub fn iter(&self) -> std::slice::Iter<'_, SelectionEntry> {
        self.entries.iter()
    }

    /// Anzahl der Einträge.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Ob der Trace leer ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Der zuletzt als [`SelectionDecision::Selected`] markierte Provider.
    #[must_use]
    pub fn selected(&self) -> Option<&ProviderName> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.decision == SelectionDecision::Selected)
            .map(|entry| &entry.provider)
    }
}

impl<'a> IntoIterator for &'a ProviderSelectionTrace {
    type Item = &'a SelectionEntry;
    type IntoIter = std::slice::Iter<'a, SelectionEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

/// Provider-Metriken als lock-freie Zähler (`Relaxed`, reine Statistik).
#[derive(Debug, Default)]
pub struct ProviderMetrics {
    attempts: AtomicU64,
    successes: AtomicU64,
    failures: AtomicU64,
    fallbacks: AtomicU64,
}

/// Konsistenter Wert-Schnappschuss von [`ProviderMetrics`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderMetricsSnapshot {
    pub attempts: u64,
    pub successes: u64,
    pub failures: u64,
    pub fallbacks: u64,
}

impl ProviderMetrics {
    /// Zählt einen Aufrufversuch.
    pub fn record_attempt(&self) {
        self.attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// Zählt einen erfolgreichen Aufruf.
    pub fn record_success(&self) {
        self.successes.fetch_add(1, Ordering::Relaxed);
    }

    /// Zählt einen fehlgeschlagenen Aufruf.
    pub fn record_failure(&self) {
        self.failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Zählt einen Wechsel auf einen Fallback-Provider.
    pub fn record_fallback(&self) {
        self.fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    /// Liest alle Zähler aus.
    #[must_use]
    pub fn snapshot(&self) -> ProviderMetricsSnapshot {
        ProviderMetricsSnapshot {
            attempts: self.attempts.load(Ordering::Relaxed),
            successes: self.successes.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            fallbacks: self.fallbacks.load(Ordering::Relaxed),
        }
    }
}

/// Mandanten-spezifische Provider-Policy: Allow-Liste je Customer.
///
/// # Description
/// Customers ohne Eintrag (und Aufrufe ohne Customer) sind unbeschränkt;
/// für Customers mit Eintrag sind nur die gelisteten Provider zulässig.
#[derive(Clone, Debug, Default)]
pub struct CustomerProviderPolicy {
    allowed: HashMap<CustomerId, HashSet<ProviderName>>,
}

impl CustomerProviderPolicy {
    /// Erlaubt `provider` für `customer` (legt die Allow-Liste bei Bedarf an).
    pub fn allow(&mut self, customer: CustomerId, provider: ProviderName) {
        self.allowed.entry(customer).or_default().insert(provider);
    }

    /// Ob für `customer` eine Einschränkung konfiguriert ist.
    #[must_use]
    pub fn is_restricted(&self, customer: &CustomerId) -> bool {
        self.allowed.contains_key(customer)
    }

    /// Ob `customer` den Provider `provider` nutzen darf.
    #[must_use]
    pub fn permits(&self, customer: Option<&CustomerId>, provider: &ProviderName) -> bool {
        match customer.and_then(|customer| self.allowed.get(customer)) {
            Some(allowed) => allowed.contains(provider),
            None => true,
        }
    }
}

/// Menge von Modell-Fähigkeiten als Bitset über [`ModelCapabilityTag`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CapabilityFlags(u8);

impl CapabilityFlags {
    fn bit(tag: ModelCapabilityTag) -> u8 {
        match tag {
            ModelCapabilityTag::Chat => 1 << 0,
            ModelCapabilityTag::Embedding => 1 << 1,
            ModelCapabilityTag::Vision => 1 << 2,
            ModelCapabilityTag::Audio => 1 << 3,
            ModelCapabilityTag::Reasoning => 1 << 4,
            ModelCapabilityTag::ToolCalling => 1 << 5,
        }
    }

    /// Nimmt eine Fähigkeit auf.
    pub fn insert(&mut self, tag: ModelCapabilityTag) {
        self.0 |= Self::bit(tag);
    }

    /// Ob die Fähigkeit enthalten ist.
    #[must_use]
    pub fn contains(self, tag: ModelCapabilityTag) -> bool {
        self.0 & Self::bit(tag) != 0
    }

    /// Ob keine Fähigkeit enthalten ist.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Capability-Matching-Matrix: Provider → Fähigkeiten seiner Modelle.
#[derive(Clone, Debug, Default)]
pub struct ProviderCapabilityMatrix {
    entries: HashMap<ProviderName, CapabilityFlags>,
}

impl ProviderCapabilityMatrix {
    /// Baut die Matrix aus den Modell-Capabilities der Records.
    #[must_use]
    pub fn from_records<'a, I>(records: I) -> Self
    where
        I: IntoIterator<Item = &'a ProviderRecord>,
    {
        let mut matrix = Self::default();
        for record in records {
            // Eintrag auch für Provider ohne Modelle anlegen (leere Flags).
            let flags = matrix.entries.entry(record.name.clone()).or_default();
            for model in &record.models {
                flags.insert(model.capability);
            }
        }
        matrix
    }

    /// Nimmt eine Fähigkeit für einen Provider auf.
    pub fn insert(&mut self, provider: ProviderName, tag: ModelCapabilityTag) {
        self.entries.entry(provider).or_default().insert(tag);
    }

    /// Fähigkeiten eines Providers, falls bekannt.
    #[must_use]
    pub fn lookup(&self, provider: &ProviderName) -> Option<CapabilityFlags> {
        self.entries.get(provider).copied()
    }

    /// Ob der Provider die Fähigkeit bietet (unbekannte Provider: `false`).
    #[must_use]
    pub fn supports(&self, provider: &ProviderName, tag: ModelCapabilityTag) -> bool {
        self.lookup(provider)
            .is_some_and(|flags| flags.contains(tag))
    }

    /// Alle Provider mit der Fähigkeit, nach Name sortiert (stabile Ausgabe).
    #[must_use]
    pub fn providers_with(&self, tag: ModelCapabilityTag) -> Vec<&ProviderName> {
        let mut providers: Vec<&ProviderName> = self
            .entries
            .iter()
            .filter(|(_, flags)| flags.contains(tag))
            .map(|(name, _)| name)
            .collect();
        providers.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        providers
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use harw_types::{CustomerId, ModelId, ModelName, ProviderId, ProviderName};
    use secrecy::SecretString;
    use url::Url;

    use super::{
        CustomerProviderPolicy, ProviderCapabilityMatrix, ProviderGovernor, ProviderMetrics,
        ProviderMetricsSnapshot, ProviderSelectionTrace, RetryPolicy, SelectionDecision,
    };
    use crate::auth::{ApiKeyConfig, ProviderAuth};
    use crate::marker::ModelCapabilityTag;
    use crate::model::{ModelRecord, ModelSettings};
    use crate::provider::{ProviderBuilder, ProviderRecord, ProviderSettings};
    use crate::test_support::{TestError, TestResult};

    fn model(name: &str, capability: ModelCapabilityTag) -> ModelRecord {
        ModelRecord {
            id: ModelId::from(name),
            name: ModelName::from(name),
            capability,
            settings: ModelSettings::default(),
        }
    }

    fn record(name: &str, models: Vec<ModelRecord>) -> TestResult<ProviderRecord> {
        Ok(ProviderBuilder::new()
            .id(ProviderId::from(name))
            .name(ProviderName::from(name))
            .base_url(Url::parse("https://api.example.com/v1")?)
            .auth(ProviderAuth::ApiKey(ApiKeyConfig {
                api_key: SecretString::new("test-key".to_owned()),
            }))
            .add_models(models)
            .build_record()?)
    }

    #[test]
    fn retry_policy_backoff_and_attempts() -> TestResult {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(3),
        };
        assert_eq!(policy.backoff_delay(0), Duration::from_secs(1));
        assert_eq!(policy.backoff_delay(1), Duration::from_secs(2));
        assert_eq!(policy.backoff_delay(2), Duration::from_secs(3));
        assert_eq!(policy.backoff_delay(u32::MAX), Duration::from_secs(3));
        assert!(policy.should_retry(2));
        assert!(!policy.should_retry(3));

        let zero = RetryPolicy {
            max_attempts: 0,
            ..policy
        };
        assert_eq!(zero.effective_attempts(), 1);

        let settings = ProviderSettings {
            max_retries: Some(2),
            ..ProviderSettings::default()
        };
        assert_eq!(RetryPolicy::from_settings(&settings).max_attempts, 3);
        assert_eq!(
            RetryPolicy::from_settings(&ProviderSettings::default()),
            RetryPolicy::default()
        );
        Ok(())
    }

    #[test]
    fn governor_respects_deny_list_and_enabled_flag() -> TestResult {
        let alpha = record("alpha", Vec::new())?;
        let beta = record("beta", Vec::new())?;
        let mut disabled = record("gamma", Vec::new())?;
        disabled.settings.enabled = Some(false);

        let mut governor = ProviderGovernor::default();
        assert!(governor.deny(ProviderName::from("beta")));
        let admitted: Vec<&str> = governor
            .filter([&alpha, &beta, &disabled])
            .into_iter()
            .map(|record| record.name.as_str())
            .collect();
        assert_eq!(admitted, vec!["alpha"]);

        assert!(governor.allow(&ProviderName::from("beta")));
        assert!(governor.admits(&beta));
        Ok(())
    }

    #[test]
    fn selection_trace_records_in_order() -> TestResult {
        let mut trace = ProviderSelectionTrace::default();
        assert!(trace.is_empty());
        trace.push(
            ProviderName::from("alpha"),
            SelectionDecision::Failed {
                reason: "timeout".to_owned(),
            },
        );
        trace.push(ProviderName::from("beta"), SelectionDecision::Selected);

        assert_eq!(trace.len(), 2);
        let names: Vec<&str> = trace.iter().map(|entry| entry.provider.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        let selected = trace
            .selected()
            .ok_or(TestError::Missing("selected provider"))?;
        assert_eq!(selected.as_str(), "beta");
        Ok(())
    }

    #[test]
    fn metrics_count_atomically() -> TestResult {
        let metrics = ProviderMetrics::default();
        metrics.record_attempt();
        metrics.record_attempt();
        metrics.record_failure();
        metrics.record_fallback();
        metrics.record_success();
        assert_eq!(
            metrics.snapshot(),
            ProviderMetricsSnapshot {
                attempts: 2,
                successes: 1,
                failures: 1,
                fallbacks: 1,
            }
        );
        Ok(())
    }

    #[test]
    fn customer_policy_restricts_only_listed_customers() -> TestResult {
        let acme = CustomerId::from("acme");
        let other = CustomerId::from("other");
        let openai = ProviderName::from("openai");
        let ollama = ProviderName::from("ollama");

        let mut policy = CustomerProviderPolicy::default();
        policy.allow(acme.clone(), openai.clone());

        assert!(policy.is_restricted(&acme));
        assert!(policy.permits(Some(&acme), &openai));
        assert!(!policy.permits(Some(&acme), &ollama));
        assert!(policy.permits(Some(&other), &ollama));
        assert!(policy.permits(None, &ollama));
        Ok(())
    }

    #[test]
    fn capability_matrix_from_records() -> TestResult {
        let alpha = record(
            "alpha",
            vec![
                model("chat", ModelCapabilityTag::Chat),
                model("vision", ModelCapabilityTag::Vision),
            ],
        )?;
        let beta = record("beta", vec![model("embed", ModelCapabilityTag::Embedding)])?;
        let empty = record("empty", Vec::new())?;

        let mut matrix = ProviderCapabilityMatrix::from_records([&alpha, &beta, &empty]);
        let alpha_name = ProviderName::from("alpha");
        assert!(matrix.supports(&alpha_name, ModelCapabilityTag::Vision));
        assert!(!matrix.supports(&alpha_name, ModelCapabilityTag::Embedding));
        let empty_flags = matrix
            .lookup(&ProviderName::from("empty"))
            .ok_or(TestError::Missing("empty provider flags"))?;
        assert!(empty_flags.is_empty());
        assert!(!matrix.supports(&ProviderName::from("unknown"), ModelCapabilityTag::Chat));

        matrix.insert(ProviderName::from("beta"), ModelCapabilityTag::Chat);
        let chat: Vec<&str> = matrix
            .providers_with(ModelCapabilityTag::Chat)
            .into_iter()
            .map(|name| name.as_str())
            .collect();
        assert_eq!(chat, vec!["alpha", "beta"]);
        Ok(())
    }
}
