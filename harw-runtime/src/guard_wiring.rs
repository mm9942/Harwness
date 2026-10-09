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

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use harw_config::ResolvedConfig;
use harw_core::{
    DriftEvent, DriftObserver, GuardPolicy, ManagedAgentSpawner, PitfallAdvisor, RoleEffortWeights,
    SessionResolvedSet,
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
/// Pitfall-Fakt genau `tool_name` als sein Werkzeug speichert (Präfix
/// `"<tool>: …"` der Beschreibung oder Schlagwort), mindestens
/// [`MIN_PITFALL_CONFIDENCE`] Konfidenz hat **und** mindestens einen
/// Argument-Schlüssel oder -Wert in seinem Text (Beschreibung + Body,
/// kleingeschrieben) enthält (Runde 7, Teil B5).
pub struct MemoryPitfallAdvisor {
    /// Die Projekt-Fakten-Wurzel, aus der Pitfall-Fakten geladen werden.
    store: Arc<FactStore>,
    /// Einmalig gefüllter Cache der Pitfall-Fakten.
    cache: OnceLock<Vec<Fact>>,
    /// Runde 9, E3: Namen der Pitfalls, die ein späterer Erfolg desselben
    /// Aufrufs gelöst hat ([`PitfallAdvisor::resolved`]); sie melden sich
    /// nicht mehr.
    resolved: Mutex<Vec<String>>,
    /// Session-getrennter, begrenzter Laufzustand für die Runtime-Aufrufer
    /// ([`PitfallAdvisor::advise_in_session`] / `resolved_in_session`): ein
    /// Erfolg in Session A löst den Hinweis nie in Session B auf.
    resolved_by_session: SessionResolvedSet,
}

impl MemoryPitfallAdvisor {
    /// Baut einen Berater für die gegebene Fakten-Wurzel.
    ///
    /// # Argumente
    /// - `store` (`Arc<FactStore>`): die Projekt-Fakten-Wurzel, typischerweise
    ///   dieselbe wie [`crate::assembly::RuntimeAssemblyBuilder::fact_stores`]
    ///   (Projekt-Anteil).
    #[must_use]
    pub fn new(store: Arc<FactStore>) -> Self {
        Self {
            store,
            cache: OnceLock::new(),
            resolved: Mutex::new(Vec::new()),
            resolved_by_session: SessionResolvedSet::new(),
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
    /// Runde 9, E3: ungebundener Legacy-Pfad (nicht von der Runtime
    /// genutzt): berücksichtigt den ungebundenen `resolved`-Zustand.
    fn advise(&self, tool_name: &str, arguments: &serde_json::Value) -> Option<String> {
        let resolved = self.resolved.lock().unwrap_or_else(PoisonError::into_inner);
        self.pitfalls()
            .iter()
            .filter(|fact| !resolved.contains(&fact.name))
            .find(|fact| pitfall_applies(fact, tool_name, arguments))
            .map(|fact| truncate_hint(&fact.description))
    }

    /// Sessiongebunden: berücksichtigt nur die Auflösungen derselben Session
    /// (nie den ungebundenen `resolved`-Zustand).
    #[must_use]
    fn advise_in_session(
        &self,
        session_id: &harw_types::SessionId,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Option<String> {
        self.pitfalls()
            .iter()
            .filter(|fact| !self.resolved_by_session.contains(session_id, &fact.name))
            .find(|fact| pitfall_applies(fact, tool_name, arguments))
            .map(|fact| truncate_hint(&fact.description))
    }

    /// Sessiongebunden: löst Treffer nur für `session_id` auf.
    fn resolved_in_session(
        &self,
        session_id: &harw_types::SessionId,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) {
        for fact in self.pitfalls() {
            if pitfall_applies(fact, tool_name, arguments) {
                self.resolved_by_session.insert(session_id, &fact.name);
            }
        }
    }

    /// Runde 9, E3: ein erfolgreicher Aufruf löst jeden Pitfall, der auf ihn
    /// passte (ungebundener Legacy-Pfad, nicht von der Runtime genutzt).
    fn resolved(&self, tool_name: &str, arguments: &serde_json::Value) {
        let mut resolved = self.resolved.lock().unwrap_or_else(PoisonError::into_inner);
        for fact in self.pitfalls() {
            if pitfall_applies(fact, tool_name, arguments) && !resolved.contains(&fact.name) {
                resolved.push(fact.name.clone());
            }
        }
    }
}

/// Mindest-Konfidenz, ab der ein Pitfall-Fakt als Wächter-Hinweis taugt
/// (Runde 7, Teil B5). Schwächere Fakten bleiben im Gedächtnis, stören aber
/// keine Werkzeugaufrufe.
pub const MIN_PITFALL_CONFIDENCE: f32 = 0.5;

/// Prüft, ob ein Pitfall-Fakt auf genau diesen Aufruf passt (Runde 7, Teil B5).
///
/// # Beschreibung
/// - Konfidenz mindestens [`MIN_PITFALL_CONFIDENCE`].
/// - Das **gespeicherte** Werkzeug des Fakts ist exakt `tool_name`
///   ([`pitfall_names_tool`]); ein bloßes Vorkommen des Namens irgendwo im
///   Text genügt nicht mehr (früher trafen so `fs.read`-Fallen auch
///   `fs.read_many` oder Fallen fremder Werkzeuge, die es nur erwähnten).
/// - Mindestens ein Argument-**Wert** (das Ziel: Pfad, Kind-ID, Befehl …)
///   kommt im Text vor. Runde 9, E3: bloße Schlüssel wie `child_id` zählen
///   nicht mehr — sonst hing ein Fehler zu einem Ziel an jedem späteren
///   Aufruf desselben Werkzeugs mit anderem Ziel.
///
/// # Arguments
/// - `fact` (`&Fact`): ein `pitfall`-Fakt.
/// - `tool_name` (`&str`): der bevorstehende Aufruf.
/// - `arguments` (`&serde_json::Value`): dessen Argumente.
///
/// # Returns
/// `true`, wenn der Hinweis an diesen Aufruf gehört.
fn pitfall_applies(fact: &Fact, tool_name: &str, arguments: &serde_json::Value) -> bool {
    // NaN zählt als „zu schwach“.
    if fact.confidence.is_nan()
        || fact.confidence < MIN_PITFALL_CONFIDENCE
        || !pitfall_names_tool(fact, tool_name)
    {
        return false;
    }
    let haystack = format!("{} {}", fact.description, fact.body).to_ascii_lowercase();
    argument_matches(&haystack, arguments)
}

/// `true`, wenn der Fakt `tool_name` als sein Werkzeug speichert.
///
/// # Beschreibung
/// Ein Pitfall nennt sein Werkzeug auf zwei Wegen (beide exakt, ohne
/// Beachtung der Groß-/Kleinschreibung):
/// - als Präfix der Beschreibung: `"<tool>: …"` (der Teil vor dem ersten
///   `:` ohne Leerraum), und/oder
/// - als Schlagwort in `tags`.
fn pitfall_names_tool(fact: &Fact, tool_name: &str) -> bool {
    let tool = tool_name.trim();
    if tool.is_empty() {
        return false;
    }
    let prefix_matches = fact
        .description
        .split_once(':')
        .map(|(head, _)| head.trim())
        .is_some_and(|head| !head.contains(char::is_whitespace) && head.eq_ignore_ascii_case(tool));
    prefix_matches
        || fact
            .tags
            .iter()
            .any(|tag| tag.trim().eq_ignore_ascii_case(tool))
}

/// Mindestlänge (Zeichen) eines Argument-Werts, damit er als Ziel zählt —
/// kürzere Werte („B", „ja") träfen fast jeden Text.
const MIN_PITFALL_VALUE_CHARS: usize = 4;

/// Prüft, ob mindestens ein Argument-Wert in `haystack` (bereits
/// kleingeschrieben) vorkommt; Schlüssel zählen nicht.
fn argument_matches(haystack: &str, arguments: &serde_json::Value) -> bool {
    match arguments {
        serde_json::Value::Object(map) => {
            map.values().any(|value| argument_matches(haystack, value))
        }
        serde_json::Value::String(text) => {
            let text = text.trim();
            text.chars().count() >= MIN_PITFALL_VALUE_CHARS
                && haystack.contains(&text.to_ascii_lowercase())
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
        // Runde 7, Teil A2.
        orchestrator_read_warn: toml
            .orchestrator_read_warn
            .unwrap_or(default.orchestrator_read_warn),
        orchestrator_read_limit: toml
            .orchestrator_read_limit
            .unwrap_or(default.orchestrator_read_limit),
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
/// Nutzerentscheidung-Rangfolge **Provider > Modell > Agent > Rolle** auf.
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

#[cfg(test)]
mod pitfall_advisor_tests {
    //! Runde 7, Teil B5: Pitfall-Hinweise nur für das gespeicherte Werkzeug,
    //! mit Mindest-Konfidenz.
    use std::sync::Arc;

    use harw_core::PitfallAdvisor;
    use harw_memory::{FactScope, FactStore};
    use serde_json::json;

    use super::MemoryPitfallAdvisor;
    use crate::test_support::{TestResult, ctx};

    /// Schreibt einen Pitfall-Fakt im Frontmatter-Format von `FactStore`.
    fn write_pitfall(
        root: &std::path::Path,
        name: &str,
        description: &str,
        tags: &str,
        confidence: f32,
    ) -> TestResult {
        let text = format!(
            "---\nname: {name}\ndescription: \"{description}\"\ntype: pitfall\nscope: project\n\
             created: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n\
             confidence: {confidence:.2}\nsources: []\ntags: [{tags}]\n---\n\nCargo.lock nie \
             von Hand ändern.\n"
        );
        std::fs::write(root.join("facts").join(format!("{name}.md")), text)?;
        Ok(())
    }

    fn advisor_with(
        facts: &[(&str, &str, &str, f32)],
    ) -> TestResult<(MemoryPitfallAdvisor, tempfile::TempDir)> {
        let dir = tempfile::tempdir()?;
        let store = FactStore::open(dir.path(), FactScope::Project).map_err(ctx("FactStore"))?;
        for (name, description, tags, confidence) in facts {
            write_pitfall(dir.path(), name, description, tags, *confidence)?;
        }
        Ok((MemoryPitfallAdvisor::new(Arc::new(store)), dir))
    }

    #[test]
    fn pitfall_hint_only_for_the_exact_stored_tool() -> TestResult {
        let (advisor, _dir) = advisor_with(&[(
            "fs-write-lock",
            "fs.write: Cargo.lock nicht überschreiben",
            "fs.write",
            0.9,
        )])?;
        let args = json!({ "path": "Cargo.lock" });
        assert!(
            advisor.advise("fs.write", &args).is_some(),
            "gleiches Werkzeug trifft"
        );
        // Früher reichte der Substring: `fs.write` steckt in `fs.write_many`,
        // und `shell.exec`-Aufrufe mit „Cargo.lock“ im Argument trafen, wenn
        // der Text das Werkzeug nur erwähnte.
        assert!(advisor.advise("fs.write_many", &args).is_none());
        assert!(advisor.advise("shell.exec", &args).is_none());
        assert!(advisor.advise("fs", &args).is_none());
        Ok(())
    }

    #[test]
    fn pitfall_mentioning_a_tool_in_its_text_is_not_bound_to_it() -> TestResult {
        let (advisor, _dir) = advisor_with(&[(
            "shell-lock",
            "shell.exec: nach fs.write Cargo.lock prüfen",
            "shell.exec",
            0.9,
        )])?;
        let args = json!({ "path": "Cargo.lock" });
        assert!(advisor.advise("fs.write", &args).is_none());
        assert!(
            advisor
                .advise("shell.exec", &json!({ "path": "Cargo.lock" }))
                .is_some()
        );
        Ok(())
    }

    /// Runde 9, E3: ein Pitfall zu einem Ziel (hier einer Kind-ID) hängt
    /// nicht an Aufrufen mit anderem Ziel — der Schlüssel allein genügt
    /// nicht — und verschwindet nach einem Erfolg für dasselbe Ziel.
    #[test]
    fn pitfall_is_scoped_to_its_target_and_cleared_by_a_success() -> TestResult {
        let (advisor, _dir) = advisor_with(&[(
            "agent-message-stale",
            "agent.message: kein eigenes, laufendes Kind mit der ID ffe02b1b child_id",
            "agent.message",
            0.9,
        )])?;
        let other = json!({ "child_id": "4ce96c7a", "text": "B" });
        assert!(
            advisor.advise("agent.message", &other).is_none(),
            "anderes Ziel, nur der Schlüssel passt"
        );
        let same = json!({ "child_id": "ffe02b1b", "text": "weiter" });
        assert!(advisor.advise("agent.message", &same).is_some());
        advisor.resolved("agent.message", &same);
        assert!(
            advisor.advise("agent.message", &same).is_none(),
            "nach einem Erfolg kein veralteter Hinweis mehr"
        );
        Ok(())
    }

    /// PL-90 H1: resolutions are scoped per session — a success in one
    /// session must not silence hints in another.
    #[test]
    fn resolve_in_session_a_does_not_suppress_the_pitfall_in_session_b() -> TestResult {
        use harw_types::SessionId;
        let (advisor, _dir) = advisor_with(&[(
            "agent-message-stale",
            "agent.message: kein eigenes, laufendes Kind mit der ID ffe02b1b child_id",
            "agent.message",
            0.9,
        )])?;
        let session_a = SessionId::from_str("a");
        let session_b = SessionId::from_str("b");
        let same = json!({ "child_id": "ffe02b1b", "text": "weiter" });
        assert!(advisor.advise_in_session(&session_a, "agent.message", &same).is_some());
        assert!(advisor.advise_in_session(&session_b, "agent.message", &same).is_some());
        // Resolve in session A only.
        advisor.resolved_in_session(&session_a, "agent.message", &same);
        assert!(
            advisor.advise_in_session(&session_a, "agent.message", &same).is_none(),
            "resolved in session A"
        );
        assert!(
            advisor.advise_in_session(&session_b, "agent.message", &same).is_some(),
            "session B unaffected by session A's resolution"
        );
        // Both directions: resolving in B now must not resurrect A's hint,
        // and a fresh session C still sees the hint.
        let session_c = SessionId::from_str("c");
        assert!(advisor.advise_in_session(&session_c, "agent.message", &same).is_some());
        Ok(())
    }

    #[test]
    fn pitfall_tag_binds_the_tool_and_low_confidence_is_ignored() -> TestResult {
        let (advisor, _dir) = advisor_with(&[
            ("tag-only", "Lockdatei-Falle", "fs.write, cargo", 0.8),
            ("schwach", "fs.edit: Cargo.lock meiden", "fs.edit", 0.3),
        ])?;
        let args = json!({ "path": "Cargo.lock" });
        assert!(advisor.advise("fs.write", &args).is_some(), "Tag bindet");
        assert!(
            advisor.advise("fs.edit", &args).is_none(),
            "unter der Mindest-Konfidenz kein Hinweis"
        );
        Ok(())
    }
}
