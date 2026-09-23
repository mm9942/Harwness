//! Verdrahtung der Wächter (Addendum F+G, `CONTRACT.md`) in die Runtime-Montage.
//!
//! # Verantwortungsbereich
//! Dieses Modul kennt drei Nähte zwischen `harw-core` (Wächter-Vertrag:
//! [`harw_core::GuardPolicy`], [`harw_core::DriftObserver`],
//! [`harw_core::PitfallAdvisor`], [`harw_core::RoleEffortWeights`]) und der
//! Montage in [`crate::assembly`]/[`crate::children`]:
//!
//! - [`DriftTracer`] implementiert [`harw_core::DriftObserver`] und meldet
//!   jedes Drift-Ereignis strukturiert über `tracing::warn!` — die Runtime
//!   hat keine weitere Senke für Wächter-Ereignisse (Persistenz übernimmt
//!   `TranscriptStateStore::record_drift`, Agent F-CORE-A).
//! - [`MemoryPitfallAdvisor`] implementiert [`harw_core::PitfallAdvisor`] und
//!   schlägt Werkzeugaufrufe gegen die `pitfall`-Fakten des Projektgedächtnisses
//!   ([`harw_memory::FactStore`]) nach.
//! - [`guard_policy_from_config`] und [`role_effort_weights_from_config`]
//!   lösen `[guards]`/`[reasoning]` aus `harw_config::HarnessConfig` gegen
//!   die jeweiligen `harw-core`-Vorgabewerte auf.
//! - [`spawn_child_reaper`] baut den periodischen Hintergrund-Task, der
//!   abgelaufene Kind-Leases einsammelt (Addendum F+G, "Zombies").
//!
//! # Nebenläufigkeit
//! [`DriftTracer`] ist ein zustandsloses Markerobjekt (`Send + Sync`).
//! [`MemoryPitfallAdvisor`] hält nur ein geteiltes `Arc<FactStore>` und einen
//! `OnceLock`-Cache; beide sind `Send + Sync`. [`spawn_child_reaper`] öffnet
//! einen `tokio`-Task (braucht eine laufende Tokio-Runtime) und läuft für die
//! Lebensdauer des Prozesses.
//!
//! # Fehler
//! Kein eigener Fehlertyp: jeder Fehlschlag (Fakten-Liste, Reap-Lauf) wird
//! nur über `tracing::warn!` gemeldet — ein Problem der Wächter-Infrastruktur
//! darf weder einen Tool-Aufruf noch den Reaper-Task scheitern lassen.

use std::sync::{Arc, OnceLock};

use harw_config::ResolvedConfig;
use harw_core::{
    DriftEvent, DriftObserver, GuardPolicy, ManagedAgentSpawner, PitfallAdvisor, RoleEffortWeights,
};
use harw_memory::{Fact, FactStore, FactType};
use harw_types::ReasoningEffort;

/// Meldet jedes Drift-Ereignis strukturiert über `tracing::warn!`.
///
/// # Beschreibung
/// Zustandsloses Markerobjekt: die einzige Aufgabe ist die Übersetzung eines
/// [`DriftEvent`] in ein strukturiertes Tracing-Feld-Set (Addendum F+G,
/// "Hinweise an das Modell nur am Ende, nie im System-Prompt" — dieser
/// Beobachter ist die reine Diagnose-Senke, nicht der Hinweistext, den der
/// Turn-Loop an das Tool-Ergebnis anhängt).
pub struct DriftTracer;

impl DriftObserver for DriftTracer {
    /// Schreibt ein Drift-Ereignis als strukturierte `tracing::warn!`-Zeile.
    ///
    /// # Argumente
    /// - `event` (`&DriftEvent`): das ausgelöste Ereignis.
    ///
    /// # Nebenläufigkeit
    /// Sicher aus jedem Thread; keine innere Veränderlichkeit.
    fn on_drift(&self, event: &DriftEvent) {
        tracing::warn!(
            kind = event.kind.key(),
            session = %event.session_id,
            tool = event.tool_name.as_deref().unwrap_or(""),
            child_role = event.child_role.as_deref().unwrap_or(""),
            detail = %event.detail,
            "harw.guard.drift"
        );
    }
}

/// Schlägt Werkzeugaufrufe gegen die `pitfall`-Fakten des Projektgedächtnisses
/// nach (Addendum F+G, `PitfallMatch`).
///
/// # Beschreibung
/// Lädt [`FactStore::list`] **einmal**, gecacht in einem `OnceLock` (Doku
/// F-RT-Brief: "Pitfall-Fakten … einmal laden, gecacht"), gefiltert auf
/// [`FactType::Pitfall`]. [`Self::advise`] meldet einen Treffer, wenn ein
/// Pitfall-Fakt `tool_name` **und** mindestens einen Argument-Schlüssel oder
/// -Wert in seinem Text (Beschreibung + Body, kleingeschrieben) enthält.
pub struct MemoryPitfallAdvisor {
    /// Die Projekt-Fakten-Wurzel, aus der Pitfall-Fakten geladen werden.
    store: Arc<FactStore>,
    /// Einmalig gefüllter Cache der Pitfall-Fakten.
    cache: OnceLock<Vec<Fact>>,
}

impl MemoryPitfallAdvisor {
    /// Baut einen Berater für die gegebene Fakten-Wurzel.
    ///
    /// # Argumente
    /// - `store` (`Arc<FactStore>`): die Projekt-Fakten-Wurzel, typischerweise
    ///   dieselbe wie [`crate::assembly::RuntimeAssemblyBuilder::fact_stores`]
    ///   (Projekt-Anteil).
    #[must_use]
    pub const fn new(store: Arc<FactStore>) -> Self {
        Self {
            store,
            cache: OnceLock::new(),
        }
    }

    /// Liefert die gecachten Pitfall-Fakten, lädt sie beim ersten Aufruf.
    ///
    /// # Beschreibung
    /// Ein Fehlschlag von [`FactStore::list`] wird nur `tracing::warn!`
    /// gemeldet; der Cache bleibt dann leer (kein Treffer, keine Wiederholung
    /// des Fehlschlags — der leere `Vec` wird ebenfalls gecacht).
    fn pitfalls(&self) -> &[Fact] {
        self.cache.get_or_init(|| match self.store.list() {
            Ok(facts) => facts
                .into_iter()
                .filter(|fact| fact.fact_type == FactType::Pitfall)
                .collect(),
            Err(error) => {
                tracing::warn!(error = %error, "guard.pitfall_advisor.list_failed");
                Vec::new()
            }
        })
    }
}

impl PitfallAdvisor for MemoryPitfallAdvisor {
    /// Prüft `tool_name`/`arguments` gegen die gecachten Pitfall-Fakten.
    ///
    /// # Argumente
    /// - `tool_name` (`&str`): der Name des unmittelbar bevorstehenden
    ///   Werkzeugaufrufs.
    /// - `arguments` (`&serde_json::Value`): dessen Argumente.
    ///
    /// # Rückgabe
    /// `Some(hint)` mit einem auf höchstens 300 Bytes gekürzten Hinweistext
    /// beim ersten Treffer; sonst `None`.
    fn advise(&self, tool_name: &str, arguments: &serde_json::Value) -> Option<String> {
        let tool_needle = tool_name.to_ascii_lowercase();
        for fact in self.pitfalls() {
            let haystack = format!("{} {}", fact.description, fact.body).to_ascii_lowercase();
            if !haystack.contains(&tool_needle) {
                continue;
            }
            if argument_matches(&haystack, arguments) {
                return Some(truncate_hint(&fact.description));
            }
        }
        None
    }
}

/// Prüft, ob mindestens ein Argument-Schlüssel oder -Wert in `haystack`
/// (bereits kleingeschrieben) vorkommt.
fn argument_matches(haystack: &str, arguments: &serde_json::Value) -> bool {
    match arguments {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            haystack.contains(&key.to_ascii_lowercase()) || argument_matches(haystack, value)
        }),
        serde_json::Value::String(text) => {
            !text.is_empty() && haystack.contains(&text.to_ascii_lowercase())
        }
        serde_json::Value::Array(items) => {
            items.iter().any(|item| argument_matches(haystack, item))
        }
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null => {
            false
        }
    }
}

/// Kürzt `text` auf höchstens 300 Bytes, an einer Zeichengrenze.
fn truncate_hint(text: &str) -> String {
    if text.len() <= 300 {
        return text.to_owned();
    }
    let mut end = 300;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Löst `[guards]` (`harw_config::HarnessConfig::guards`) gegen
/// [`GuardPolicy::default`] auf.
///
/// # Beschreibung
/// Jedes `None`-Feld in `config.harness.guards` fällt auf den entsprechenden
/// Vorgabewert zurück; kein Feld kann fehlschlagen (alles `u32`/`bool`).
///
/// # Argumente
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration des Laufs.
#[must_use]
pub fn guard_policy_from_config(config: &ResolvedConfig) -> GuardPolicy {
    let toml = &config.harness.guards;
    let default = GuardPolicy::default();
    GuardPolicy {
        enabled: toml.enabled.unwrap_or(default.enabled),
        repeated_failure_warn: toml
            .repeated_failure_warn
            .unwrap_or(default.repeated_failure_warn),
        repeated_failure_abort: toml
            .repeated_failure_abort
            .unwrap_or(default.repeated_failure_abort),
        no_progress_rounds_warn: toml
            .no_progress_rounds_warn
            .unwrap_or(default.no_progress_rounds_warn),
        no_progress_rounds_abort: toml
            .no_progress_rounds_abort
            .unwrap_or(default.no_progress_rounds_abort),
        plan_stale_rounds: toml.plan_stale_rounds.unwrap_or(default.plan_stale_rounds),
    }
}

/// Löst `[reasoning]` (`harw_config::HarnessConfig::reasoning`) gegen
/// [`RoleEffortWeights::default`] auf.
///
/// # Beschreibung
/// Jedes Feld wird, falls gesetzt, über `str::parse::<ReasoningEffort>`
/// aufgelöst; ein ungültiges Label (kein `minimal|low|medium|high|xhigh|max`)
/// wird `tracing::warn!`-gemeldet und fällt auf den Vorgabewert zurück,
/// genau wie ein fehlendes Feld.
///
/// # Argumente
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration des Laufs.
#[must_use]
pub fn role_effort_weights_from_config(config: &ResolvedConfig) -> RoleEffortWeights {
    let toml = &config.harness.reasoning;
    let default = RoleEffortWeights::default();
    RoleEffortWeights {
        uia: parse_effort_field(toml.uia.as_deref(), default.uia, "uia"),
        root_orchestrator: parse_effort_field(
            toml.root_orchestrator.as_deref(),
            default.root_orchestrator,
            "root_orchestrator",
        ),
        root_orchestrator_with_subs: parse_effort_field(
            toml.root_orchestrator_with_subs.as_deref(),
            default.root_orchestrator_with_subs,
            "root_orchestrator_with_subs",
        ),
        sub_orchestrator: parse_effort_field(
            toml.sub_orchestrator.as_deref(),
            default.sub_orchestrator,
            "sub_orchestrator",
        ),
        worker_complex: parse_effort_field(
            toml.worker_complex.as_deref(),
            default.worker_complex,
            "worker_complex",
        ),
        worker_simple: parse_effort_field(
            toml.worker_simple.as_deref(),
            default.worker_simple,
            "worker_simple",
        ),
    }
}

/// Parst ein einzelnes `[reasoning]`-Feld, fällt bei `None` oder ungültigem
/// Label auf `default` zurück (`field` nur für die Warnung).
fn parse_effort_field(
    label: Option<&str>,
    default: ReasoningEffort,
    field: &str,
) -> ReasoningEffort {
    let Some(label) = label else {
        return default;
    };
    match label.parse::<ReasoningEffort>() {
        Ok(effort) => effort,
        Err(error) => {
            tracing::warn!(field, label, error = %error, "runtime.reasoning_weights.invalid_label");
            default
        }
    }
}

/// Löst den Standard-Reasoning-Effort eines Kindes nach der
/// Nutzerentscheidung-Rangfolge **Provider > Modell > Agent > Rolle** auf
/// (Welle 8, `recursive-cooking-lobster.md`).
///
/// # Beschreibung
/// Prüft die vier Ebenen in genau dieser Reihenfolge und liefert den ersten
/// `Some`-Wert:
///
/// 1. `provider_default` — `ProviderToml.default_reasoning_effort`
///    (`harw-config`), bereits typisiert.
/// 2. `model_default` — `ModelToml.default_reasoning_effort`
///    (`harw-config`), bereits typisiert.
/// 3. `agent_default` — `ExecutableAgentIr::reasoning_effort()`
///    (`harw-agent-dsl`), ein undurchsichtiges Label. Wird hier per
///    `str::parse::<ReasoningEffort>` typisiert; ein unbekanntes Label ist
///    **kein Abbruch** — es wird `tracing::warn!`-gemeldet und diese Ebene
///    übersprungen (fällt zur Rollen-Ebene durch), analog zu
///    [`parse_effort_field`] für `[reasoning]`.
/// 4. `role_default` — der Rollen-Standard aus
///    [`role_effort_weights_from_config`] (`RoleEffortWeights`), der Boden
///    dieser Rangfolge.
///
/// Eine explizite Live-Einstellung (`/effort` in der Sitzung) ist **nicht**
/// Teil dieser Funktion — sie steht laut Nutzerentscheidung über allen vier
/// Ebenen und muss vom Aufrufer bereits vor dem Aufruf dieser Funktion
/// abgefangen werden (diese Funktion liefert nur den *Standard*, keinen
/// Live-Override).
///
/// # Argumente
/// - `provider_default` (`Option<ReasoningEffort>`): der Provider-Standard
///   des tatsächlich für diese Rolle aufgelösten Providers.
/// - `model_default` (`Option<ReasoningEffort>`): der Modell-Standard des
///   tatsächlich für diese Rolle aufgelösten Modells.
/// - `agent_default` (`Option<&str>`): `ExecutableAgentIr::reasoning_effort()`
///   des spawnenden Kindes — ein undurchsichtiges DSL-Label, hier typisiert.
/// - `role_default` (`Option<ReasoningEffort>`): der Rollen-Standard (z. B.
///   aus `RoleEffortWeights`); üblicherweise `Some`, da
///   [`role_effort_weights_from_config`] bereits auf `RoleEffortWeights::default`
///   zurückfällt — als `Option` geführt, damit ein Aufrufer ohne aufgelöste
///   Rolle (kein Rollen-Kontext) `None` übergeben kann.
///
/// # Rückgabe
/// `Some(ReasoningEffort)` mit der ersten Ebene, die eine gültige Aussage
/// trifft (in der Rangfolge Provider → Modell → Agent → Rolle); `None`, wenn
/// keine der vier Ebenen eine (gültige) Aussage trifft.
///
/// # Nebenläufigkeit
/// Rein; von jedem Thread aus sicher.
#[must_use]
pub fn resolve_default_reasoning_effort(
    provider_default: Option<ReasoningEffort>,
    model_default: Option<ReasoningEffort>,
    agent_default: Option<&str>,
    role_default: Option<ReasoningEffort>,
) -> Option<ReasoningEffort> {
    if let Some(effort) = provider_default {
        return Some(effort);
    }
    if let Some(effort) = model_default {
        return Some(effort);
    }
    if let Some(label) = agent_default {
        match label.parse::<ReasoningEffort>() {
            Ok(effort) => return Some(effort),
            Err(error) => {
                tracing::warn!(
                    label,
                    error = %error,
                    "runtime.reasoning_default.invalid_agent_label"
                );
                // Fällt bewusst zur Rollen-Ebene durch, statt abzubrechen.
            }
        }
    }
    role_default
}

/// Baut den periodischen Kind-Reaper (Addendum F+G, "Zombies").
///
/// # Beschreibung
/// Ruft beim Start einmal `spawner.reconcile_expired_leases` (Bestandsaufnahme
/// bereits vor diesem Prozess abgelaufener Leases), dann alle `interval`
/// Sekunden `spawner.reap`. **Zustellung:** diese Runtime hat keinen
/// erreichten Aufrufer von `harw_core::resume_after_child_durable` (geprüft:
/// weder `harw-runtime`, `harw-tui` noch `harw-cli` rufen es derzeit auf) —
/// abgelaufene Kinder werden deshalb nur über `tracing::warn!` gemeldet, nicht
/// an ihre Eltern zugestellt. **BLOCKED-Notiz für den Contract-Owner:** ein
/// echter Zustellweg (Aufruf von `resume_after_child_durable` mit dem
/// korrelierten `parent`/`handoff_call_id` aus [`harw_core::ReapReport::expired`])
/// fehlt in der Runtime und muss von einem Folge-Auftrag ergänzt werden.
///
/// # Argumente
/// - `spawner` (`Arc<ManagedAgentSpawner>`): derselbe Spawner, den
///   [`crate::assembly`] beim Bau der Montage erzeugt.
/// - `interval` (`std::time::Duration`): der Abstand zwischen zwei
///   Reap-Läufen (Vorgabe des Aufrufers: 30 s).
///
/// # Rückgabe
/// Den `JoinHandle` des Hintergrund-Tasks; der Aufrufer hält ihn typischerweise
/// nicht (der Task läuft für die Lebensdauer des Prozesses), kann ihn aber
/// zum Abbruch verwenden.
///
/// # Nebenläufigkeit
/// Braucht eine laufende Tokio-Runtime (`tokio::runtime::Handle::try_current`
/// im Aufrufer prüfen, bevor dieser Baustein montiert wird).
pub fn spawn_child_reaper(
    spawner: Arc<ManagedAgentSpawner>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        match spawner.reconcile_expired_leases(jiff::Timestamp::now()) {
            Ok(expired) => {
                for child in &expired {
                    tracing::warn!(
                        child = %child.child,
                        parent = %child.parent,
                        role = %child.role,
                        "runtime.child_reaper.startup_reconcile_undelivered"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "runtime.child_reaper.reconcile_failed");
            }
        }
        let mut ticker = tokio::time::interval(interval);
        // Der erste `tick()` feuert sofort; das ist hier gewollt (ein Reap
        // direkt nach der Reconciliation ist harmlos, `reap` ist idempotent
        // gegenüber bereits geräumten Kindern).
        loop {
            ticker.tick().await;
            match spawner.reap(jiff::Timestamp::now()) {
                Ok(report) => {
                    for expired in &report.expired {
                        tracing::warn!(
                            child = %expired.child,
                            parent = %expired.parent,
                            role = %expired.role,
                            "runtime.child_reaper.lease_expired_undelivered"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, "runtime.child_reaper.reap_failed");
                }
            }
        }
    })
}

#[cfg(test)]
mod resolve_default_reasoning_effort_tests {
    //! Tests für [`resolve_default_reasoning_effort`] — je Rangfolge-Ebene
    //! (Provider > Modell > Agent > Rolle, Welle 8) ein Test, plus die
    //! Fail-Open-Behandlung eines ungültigen Agenten-Labels.
    use super::resolve_default_reasoning_effort;
    use harw_types::ReasoningEffort;

    #[test]
    fn test_role_only_uses_role_default() {
        let result = resolve_default_reasoning_effort(None, None, None, Some(ReasoningEffort::Low));
        assert_eq!(result, Some(ReasoningEffort::Low));
    }

    #[test]
    fn test_agent_overrides_role() {
        let result =
            resolve_default_reasoning_effort(None, None, Some("high"), Some(ReasoningEffort::Low));
        assert_eq!(result, Some(ReasoningEffort::High));
    }

    #[test]
    fn test_model_overrides_agent_and_role() {
        let result = resolve_default_reasoning_effort(
            None,
            Some(ReasoningEffort::Medium),
            Some("high"),
            Some(ReasoningEffort::Low),
        );
        assert_eq!(result, Some(ReasoningEffort::Medium));
    }

    #[test]
    fn test_provider_overrides_model_agent_and_role_even_when_all_conflict() {
        let result = resolve_default_reasoning_effort(
            Some(ReasoningEffort::Max),
            Some(ReasoningEffort::Medium),
            Some("high"),
            Some(ReasoningEffort::Low),
        );
        assert_eq!(result, Some(ReasoningEffort::Max));
    }

    #[test]
    fn test_invalid_agent_label_skips_to_role_default() {
        let result = resolve_default_reasoning_effort(
            None,
            None,
            Some("not-a-real-effort-level"),
            Some(ReasoningEffort::Low),
        );
        assert_eq!(
            result,
            Some(ReasoningEffort::Low),
            "an unparsable agent label must warn and fall through to the role default, not abort"
        );
    }

    #[test]
    fn test_invalid_agent_label_with_no_role_default_yields_none() {
        let result = resolve_default_reasoning_effort(None, None, Some("bogus"), None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_all_none_yields_none() {
        let result = resolve_default_reasoning_effort(None, None, None, None);
        assert_eq!(result, None);
    }
}
