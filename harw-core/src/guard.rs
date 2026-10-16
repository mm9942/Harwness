//! Wächter für Drift, Zombies und Endlosschleifen — ohne zusätzliche
//! Modellaufrufe (Addendum F+G, Abschnitt "Agent F-CORE-A").
//!
//! # Zweck
//! Dieses Modul beobachtet, was innerhalb eines Turns tatsächlich passiert
//! (Tool-Ergebnisse, Modellrunden) und erkennt daraus rein deterministisch
//! sechs Drift-Arten: wiederholt fehlschlagende Aufrufe, Runden ohne
//! Fortschritt, veraltete Pläne, Abweichung vom Plan-Umfang, Pitfall-Treffer
//! und — von `harw-core/src/child_controller.rs` gemeldet — doppelte
//! Delegation sowie Kind-Budget-/Lease-Verstöße.
//!
//! # Verantwortungsgrenze
//! [`TurnGuard`] trägt nur die Turn-lokale Zustandsmaschine; sie ruft weder
//! den `StateStore` noch einen [`DriftObserver`] selbst auf — das erledigt
//! `harw-core/src/turn_loop.rs` nach jedem Verdikt. [`PitfallAdvisor`] und
//! [`ProgressObserver`] sind reine Schnittstellen; ihre Implementierungen
//! leben in `harw-runtime` (Knoten F-RT).
//!
//! # Concurrency
//! [`DriftObserver`], [`PitfallAdvisor`] und [`ProgressObserver`] sind
//! `Send + Sync` und werden über `Arc<dyn ...>` geteilt. [`TurnGuard`] selbst
//! ist reiner Turn-lokaler Zustand ohne innere Veränderlichkeit über
//! `.await`-Punkte hinweg und wird nicht zwischen Threads geteilt.
//!
//! # Fehlerarten
//! Dieses Modul selbst erzeugt keine Fehler — Beobachtungsmethoden geben ein
//! [`GuardVerdict`] zurück, nie ein `Result`.
//!
//! # Examples
//! ```rust
//! use harw_core::capture::ToolOutcomeStatus;
//! use harw_core::guard::{GuardPolicy, GuardVerdict, TurnGuard};
//! use harw_types::SessionId;
//!
//! let session_id = SessionId::new();
//! let mut guard = TurnGuard::new(GuardPolicy::default(), &session_id);
//! let args = serde_json::json!({});
//! let verdict = guard.observe_tool_result(
//!     "fs.read",
//!     &args,
//!     ToolOutcomeStatus::Success,
//!     "ok",
//! );
//! assert!(matches!(verdict, GuardVerdict::Continue));
//! ```

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::capture::ToolOutcomeStatus;

/// Art einer erkannten Drift.
///
/// # Description
/// `key()` liefert den stabilen, serde-kompatiblen Bezeichner (identisch mit
/// dem `#[serde(rename_all = "snake_case")]`-Namen), wie er in
/// `harw_session_store::meta::SessionMeta::drift_events` als Schlüssel und in
/// Log-Feldern verwendet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// Derselbe Tool-Aufruf (Name + kanonische Argumente) schlägt wiederholt fehl.
    RepeatedFailingCall,
    /// Mehrere Modellrunden in Folge ohne erkennbaren Fortschritt.
    NoProgressRounds,
    /// Lange keine `plan.*`-Aktualisierung mehr, obwohl ein Plan existiert.
    PlanStale,
    /// Eine Dateiänderung betrifft einen Pfad, der im zuletzt bekannten Plan
    /// nicht vorkommt.
    PlanScopeDrift,
    /// Ein registrierter [`PitfallAdvisor`] hat für diesen Aufruf gewarnt.
    PitfallMatch,
    /// Dieselbe Delegation (Rolle + normalisierter Auftragstext) wurde vom
    /// selben Elternteil kürzlich bereits vergeben.
    DuplicateDelegation,
    /// Ein Kind hat sein Token-Budget beim Abschluss überschritten.
    ChildOverBudget,
    /// Die Lease eines Kindes ist abgelaufen, ohne dass es geerntet wurde.
    ChildLeaseExpired,
    /// Runde 7, Teil A2: ein Orchestrator hat sein Lesebudget (eigene
    /// Lesezugriffe über die ganze Kind-Sitzung) erreicht bzw. überschritten.
    ReadBudget,
    /// Runde 7, Teil A5: `agent.status` wurde für dieselbe `child_id`
    /// innerhalb kurzer Zeit erneut abgefragt (Polling statt Warten auf die
    /// Benachrichtigung).
    StatusPolling,
}

impl DriftKind {
    /// Liefert den stabilen, serde-kompatiblen Bezeichner dieser Drift-Art.
    ///
    /// # Returns
    /// Der `snake_case`-Name, identisch mit der serde-Repräsentation.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            DriftKind::RepeatedFailingCall => "repeated_failing_call",
            DriftKind::NoProgressRounds => "no_progress_rounds",
            DriftKind::PlanStale => "plan_stale",
            DriftKind::PlanScopeDrift => "plan_scope_drift",
            DriftKind::PitfallMatch => "pitfall_match",
            DriftKind::DuplicateDelegation => "duplicate_delegation",
            DriftKind::ChildOverBudget => "child_over_budget",
            DriftKind::ChildLeaseExpired => "child_lease_expired",
            DriftKind::ReadBudget => "read_budget",
            DriftKind::StatusPolling => "status_polling",
        }
    }
}

/// Ein einzelnes erkanntes Drift-Ereignis.
///
/// # Description
/// Trägt genug Kontext, um ohne Rückgriff auf den Turn-Zustand
/// nachvollziehbar zu sein: welche Session, welche Art, ein Freitext-Detail
/// und optional der betroffene Tool-Name bzw. die betroffene Kind-Rolle
/// (nur für [`DriftKind::ChildOverBudget`]/[`DriftKind::ChildLeaseExpired`]/
/// [`DriftKind::DuplicateDelegation`] relevant).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftEvent {
    pub kind: DriftKind,
    pub session_id: String,
    pub detail: String,
    pub tool_name: Option<String>,
    pub child_role: Option<String>,
}

/// Wird über jedes erkannte Drift-Ereignis benachrichtigt.
///
/// # Concurrency
/// `Send + Sync`; wird aus dem Turn-Loop-Hot-Path synchron aufgerufen und
/// darf nicht blockieren.
pub trait DriftObserver: Send + Sync {
    /// Wird für jedes Drift-Ereignis genau einmal aufgerufen, nachdem es
    /// erkannt wurde.
    fn on_drift(&self, event: &DriftEvent);
}

/// Liefert eine deterministische Warnung zu einem bevorstehenden Tool-Aufruf,
/// gespeist aus dem Projektgedächtnis (`Pitfall`-Fakten).
///
/// # Concurrency
/// `Send + Sync`; wird synchron vor jeder Werkzeugausführung befragt.
pub trait PitfallAdvisor: Send + Sync {
    /// Prüft, ob zu `tool_name`/`arguments` ein bekannter Pitfall passt.
    ///
    /// # Returns
    /// `Some(hinweis)`, falls ein Fakt zutrifft (Hinweistext ≤ 300 Bytes lt.
    /// Vertrag der Implementierung); sonst `None`.
    /// Legacy session-less advise (PL-90 H1, deprecated for stateful
    /// advisors): called with no session binding, so a shared advisor cannot
    /// isolate per-session resolution state. Runtime callers must use
    /// [`PitfallAdvisor::advise_in_session`] instead; stateless
    /// implementations should keep delegating to this method from
    /// `advise_in_session`.
    fn advise(&self, tool_name: &str, arguments: &serde_json::Value) -> Option<String>;

    /// Sessiongebundene Variante. Ein gemeinsam montierter Advisor darf
    /// den privaten Laufzustand einer anderen Session nicht beeinflussen.
    /// Bestehende zustandslose Implementierungen bleiben kompatibel.
    #[must_use]
    fn advise_in_session(
        &self,
        _session_id: &harw_types::SessionId,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Option<String> {
        self.advise(tool_name, arguments)
    }

    /// Legacy-Aufruf ohne Sessionbindung; neue Runtime-Aufrufer verwenden
    /// resolved_in_session und dürfen diesen Aufruf nicht direkt benutzen.
    /// Legacy session-less resolution (PL-90 H1, deprecated for stateful
    /// advisors): with no session binding, a shared advisor writes into a
    /// global bucket, so one session's success can silence hints in another.
    /// Runtime callers must use [`PitfallAdvisor::resolved_in_session`] and
    /// must not call this directly.
    fn resolved(&self, _tool_name: &str, _arguments: &serde_json::Value) {}

    /// Der Erfolg darf nur einen Hinweis in derselben Session auflösen.
    /// Ein bestätigter, dauerhaft gültiger Fix gehört stattdessen durch
    /// die separate Learning-/Verification-Gate in den Knowledge-Store.
    ///
    /// Der Default tut bewusst **nichts** (fail-safe): ein Advisor, der
    /// diese Methode nicht überschreibt, kann Auflösungen nicht pro Session
    /// führen, und eine Weiterleitung an das ungebundene [`Self::resolved`]
    /// würde den Erfolg einer Session für alle anderen wirksam machen. Ein
    /// verbleibender Hinweis ist nur ein Hinweis; ein zu Unrecht
    /// unterdrückter ist ein Vertrauensbruch zwischen Sessions.
    fn resolved_in_session(
        &self,
        _session_id: &harw_types::SessionId,
        _tool_name: &str,
        _arguments: &serde_json::Value,
    ) {
    }
}

/// Standard-Obergrenze für gleichzeitig geführte Sessions in
/// [`SessionResolvedSet`].
pub const SESSION_RESOLVED_MAX_SESSIONS: usize = 256;
/// Standard-Obergrenze für aufgelöste Hinweise je Session in
/// [`SessionResolvedSet`].
pub const SESSION_RESOLVED_MAX_ENTRIES: usize = 64;

/// Je Session getrennte, begrenzte Menge „aufgelöster“ Hinweisnamen.
///
/// # Beschreibung
/// Bausteine für [`PitfallAdvisor`]-Implementierungen, die in einer
/// gemeinsam montierten Instanz Laufzustand pro Session führen müssen.
/// Eine Auflösung in Session A ist für Session B unsichtbar. Beide
/// Dimensionen sind begrenzt: über [`Self::with_limits`] hinaus verdrängt die
/// jeweils älteste Session bzw. der älteste Eintrag. Verdrängung macht einen
/// Hinweis wieder sichtbar (fail-open für Hinweise, nie für Sessions).
///
/// # Concurrency
/// `Send + Sync`; ein interner `Mutex`, nie über `.await` gehalten.
#[derive(Debug)]
pub struct SessionResolvedSet {
    max_sessions: usize,
    max_entries: usize,
    inner: std::sync::Mutex<SessionResolvedInner>,
}

#[derive(Debug, Default)]
struct SessionResolvedInner {
    /// Einfüge-Reihenfolge der Sessions (älteste vorn).
    order: std::collections::VecDeque<harw_types::SessionId>,
    names: HashMap<harw_types::SessionId, std::collections::VecDeque<String>>,
}

impl Default for SessionResolvedSet {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionResolvedSet {
    /// Leere Menge mit den Standard-Obergrenzen.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(SESSION_RESOLVED_MAX_SESSIONS, SESSION_RESOLVED_MAX_ENTRIES)
    }

    /// Leere Menge; beide Grenzen werden auf mindestens 1 angehoben.
    #[must_use]
    pub fn with_limits(max_sessions: usize, max_entries: usize) -> Self {
        Self {
            max_sessions: max_sessions.max(1),
            max_entries: max_entries.max(1),
            inner: std::sync::Mutex::new(SessionResolvedInner::default()),
        }
    }

    /// `true`, wenn `name` in genau dieser Session aufgelöst wurde.
    #[must_use]
    pub fn contains(&self, session_id: &harw_types::SessionId, name: &str) -> bool {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .names
            .get(session_id)
            .is_some_and(|names| names.iter().any(|existing| existing == name))
    }

    /// Merkt `name` für `session_id` vor; verdrängt bei Bedarf das Älteste.
    pub fn insert(&self, session_id: &harw_types::SessionId, name: &str) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !inner.names.contains_key(session_id) {
            while inner.names.len() >= self.max_sessions {
                let Some(oldest) = inner.order.pop_front() else {
                    break;
                };
                inner.names.remove(&oldest);
            }
            inner.order.push_back(session_id.clone());
        }
        let names = inner.names.entry(session_id.clone()).or_default();
        if names.iter().any(|existing| existing == name) {
            return;
        }
        while names.len() >= self.max_entries {
            names.pop_front();
        }
        names.push_back(name.to_owned());
    }

    /// Vergisst eine Session (z. B. nach ihrem Ende).
    pub fn forget(&self, session_id: &harw_types::SessionId) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.names.remove(session_id);
        inner.order.retain(|id| id != session_id);
    }
}

/// Wird nach jeder Modellrunde und jedem Tool-Ergebnis benachrichtigt, damit
/// ein Lease-/Budget-Wächter (`harw-core/src/child_controller.rs`) erkennen
/// kann, dass eine Kind-Session noch aktiv arbeitet.
///
/// # Concurrency
/// `Send + Sync`; wird synchron aus dem Turn-Loop-Hot-Path aufgerufen.
pub trait ProgressObserver: Send + Sync {
    /// Meldet, dass `session_id` gerade Fortschritt gemacht hat.
    fn on_progress(&self, session_id: &harw_types::SessionId);

    /// Meldet die Token-Nutzung einer gerade abgeschlossenen Modell-Runde.
    fn on_round_usage(&self, _session_id: &harw_types::SessionId, _usage: &harw_types::TokenUsage) {
    }

    /// Meldet einen abgeschlossenen Tool-Aufruf.
    fn on_tool_call(&self, _session_id: &harw_types::SessionId) {}

    /// Runde 5, Teil M: meldet an einer Runden-Grenze den jüngsten
    /// Assistententext der Sitzung (Aktivitätsjournal des Kindes).
    fn on_assistant_text(&self, _session_id: &harw_types::SessionId, _text: &str) {}

    /// Runde 5, Teil M: entnimmt an einer Runden-Grenze die wartenden
    /// Nachrichten des Elternteils (`agent.message`) bzw. der Kinder
    /// (`parent.message`) für diese Sitzung. Standard: keine.
    fn take_inbound_messages(&self, _session_id: &harw_types::SessionId) -> Vec<String> {
        Vec::new()
    }
}

/// Schwellenwerte und Ein/Aus-Schalter der Turn-Wächter.
///
/// # Concurrency
/// `Copy`-Datenhalter ohne innere Veränderlichkeit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardPolicy {
    /// Schaltet alle Wächter dieses Moduls ein/aus.
    pub enabled: bool,
    /// Fehlerzahl derselben Signatur, ab der einmal gewarnt wird.
    pub repeated_failure_warn: u32,
    /// Fehlerzahl derselben Signatur, ab der der Turn abgebrochen wird.
    pub repeated_failure_abort: u32,
    /// Anzahl Runden ohne Fortschritt, ab der gewarnt wird.
    pub no_progress_rounds_warn: u32,
    /// Anzahl Runden ohne Fortschritt, ab der der Turn abgebrochen wird.
    pub no_progress_rounds_abort: u32,
    /// Anzahl Runden ohne `plan.*`-Aufruf, ab der einmal gewarnt wird.
    pub plan_stale_rounds: u32,
    /// Runde 7, Teil A2: eigene Lesezugriffe eines Orchestrators (über die
    /// ganze Kind-Sitzung), ab denen jeder weitere Lesezugriff einen Hinweis
    /// bekommt. Gilt nur für Orchestrator-Rollen.
    pub orchestrator_read_warn: u32,
    /// Runde 7, Teil A2: höchstens so viele eigene Lesezugriffe darf ein
    /// Orchestrator ausführen; jeder weitere wird abgelehnt. `0` schaltet das
    /// Lesebudget ab.
    pub orchestrator_read_limit: u32,
}

impl Default for GuardPolicy {
    /// Nutzerentscheidung (Addendum F+G): aktiviert, 2/3 für wiederholte
    /// Fehler, 4/8 für fehlenden Fortschritt, 6 Runden bis `PlanStale`.
    /// Runde 7, Teil A2: Orchestratoren lesen höchstens 5-mal selbst
    /// (Hinweis ab dem 4. Zugriff).
    fn default() -> Self {
        Self {
            enabled: true,
            repeated_failure_warn: 2,
            repeated_failure_abort: 3,
            no_progress_rounds_warn: 4,
            no_progress_rounds_abort: 8,
            plan_stale_rounds: 6,
            orchestrator_read_warn: 4,
            orchestrator_read_limit: 5,
        }
    }
}

/// Ergebnis einer Wächter-Beobachtung.
#[derive(Debug)]
pub enum GuardVerdict {
    /// Kein Befund; der Turn läuft unverändert weiter.
    Continue,
    /// Ein Befund, der dem Modell als Hinweis mitgegeben wird, den Turn aber
    /// nicht beendet.
    Warn { event: DriftEvent, hint: String },
    /// Ein Befund, der den Turn sofort beendet (wie ein ausgeschöpftes
    /// `max_model_rounds`-Budget).
    Abort { event: DriftEvent, hint: String },
}

// Baut die kanonische, deterministische Textform eines `serde_json::Value`
// für die Signaturbildung — Objektschlüssel werden sortiert, damit dieselben
// Argumente unabhängig von ihrer Einfügereihenfolge dieselbe Signatur ergeben.
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let rendered: Vec<String> = entries
                .into_iter()
                .map(|(key, val)| {
                    let key_json = serde_json::to_string(key).unwrap_or_default();
                    format!("{key_json}:{}", canonical_json(val))
                })
                .collect();
            format!("{{{}}}", rendered.join(","))
        }
        serde_json::Value::Array(items) => {
            let rendered: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", rendered.join(","))
        }
        other => other.to_string(),
    }
}

/// Signatur eines Tool-Aufrufs: Name + kanonische Argumente.
///
/// # Description
/// Dieselbe Bildungsregel, die [`TurnGuard`] intern für Fehlerzähler und
/// gesehene Erfolgs-Signaturen verwendet — `pub(crate)`, damit
/// `harw-core/src/turn_loop.rs` dieselbe Signatur für die
/// Fortschritts-Erkennung einer Runde (neue Erfolgs-Signatur) bilden kann,
/// ohne die Bildungsregel zu duplizieren.
///
/// # Arguments
/// - `tool_name` (`&str`): Name des Tools.
/// - `arguments` (`&serde_json::Value`): die vom Modell übergebenen Argumente.
///
/// # Returns
/// Ein deterministischer, für identische Eingaben stabiler Signatur-String.
pub(crate) fn call_signature(tool_name: &str, arguments: &serde_json::Value) -> String {
    format!("{tool_name}\u{1f}{}", canonical_json(arguments))
}

// Letztes Segment eines Pfad-Strings (Dateiname), ohne Verzeichnisanteil.
fn file_name_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

const PLAN_SCOPE_TOOLS: [&str; 3] = ["fs.write", "fs.edit", "fs.patch"];

/// Tools, deren wiederholter *erfolgreicher* Aufruf mit identischer Signatur
/// dennoch als Rundenfortschritt zählt (Fix C, Moduldoku „Achter Nachtrag").
///
/// # Description
/// `harw-core/src/turn_loop.rs::apply_tool_guard` verwendet diese Liste, um
/// die sonst geltende Regel „nur eine *neue* Erfolgs-Signatur zählt als
/// Fortschritt" für Werkzeuge auszusetzen, mit denen legitim auf einen
/// laufenden Hintergrundprozess gewartet wird (z. B. wiederholtes
/// `shell.exec status`). Ohne diese Ausnahme erschien ein solches Polling dem
/// Modell als Endlosschleife ohne Fortschritt, obwohl jeder Aufruf real
/// ausgeführt wurde — siehe die umformulierte Hinweismeldung in
/// [`TurnGuard::observe_round_end`].
pub(crate) const POLLING_TOOLS: &[&str] = &["shell.exec"];

/// Turn-lokaler Zustand der Wächter aus Addendum F+G.
///
/// # Description
/// Hält je Turn: Fehlerzähler pro Aufruf-Signatur, bereits gesehene
/// Erfolgs-Signaturen, Runden ohne Fortschritt, Runden seit dem letzten
/// `plan.*`-Aufruf und den zuletzt bekannten Plantext. Wird pro Turn neu
/// erzeugt ([`TurnGuard::new`]) — es gibt keine Turn-übergreifende
/// Persistenz dieses Zustands.
///
/// # Concurrency
/// Kein `Send`/`Sync` erforderlich: lebt ausschließlich auf dem Stack von
/// `harw-core/src/turn_loop.rs::drive_turn`.
pub struct TurnGuard {
    policy: GuardPolicy,
    session_id: harw_types::SessionId,
    failure_counts: HashMap<String, u32>,
    seen_success_signatures: HashSet<String>,
    rounds_without_progress: u32,
    rounds_since_last_plan_call: u32,
    last_plan_text: Option<String>,
    plan_stale_warned: bool,
    scope_drift_warned_paths: HashSet<String>,
}

impl TurnGuard {
    /// Erzeugt einen frischen Wächter für einen einzelnen Turn.
    ///
    /// # Arguments
    /// - `policy` (`GuardPolicy`): Schwellenwerte dieses Turns.
    /// - `session_id` (`&harw_types::SessionId`): Session, deren Turn
    ///   beobachtet wird — wird in jedes erzeugte [`DriftEvent`] kopiert.
    #[must_use]
    pub fn new(policy: GuardPolicy, session_id: &harw_types::SessionId) -> Self {
        Self {
            policy,
            session_id: session_id.clone(),
            failure_counts: HashMap::new(),
            seen_success_signatures: HashSet::new(),
            rounds_without_progress: 0,
            rounds_since_last_plan_call: 0,
            last_plan_text: None,
            plan_stale_warned: false,
            scope_drift_warned_paths: HashSet::new(),
        }
    }

    fn event(
        &self,
        kind: DriftKind,
        detail: impl Into<String>,
        tool_name: Option<&str>,
    ) -> DriftEvent {
        DriftEvent {
            kind,
            session_id: self.session_id.to_string(),
            detail: detail.into(),
            tool_name: tool_name.map(ToOwned::to_owned),
            child_role: None,
        }
    }

    /// Wertet das Ergebnis eines einzelnen, bereits ausgeführten Tool-Aufrufs
    /// aus.
    ///
    /// # Description
    /// Bei `status == Error` wird der Fehlerzähler der Aufruf-Signatur
    /// (Name + kanonische Argumente) erhöht: ab `repeated_failure_warn`
    /// Fehlern derselben Signatur wird gewarnt, ab `repeated_failure_abort`
    /// der Turn abgebrochen. Bei `status == Success` wird der Fehlerzähler
    /// der Signatur zurückgesetzt (ein erfolgreicher Aufruf beendet die
    /// Fehlserie) und die Signatur als „diesen Turn gesehen" vermerkt.
    ///
    /// `plan.*`-Aufrufe (Präfix `"plan."`) merken bei Erfolg `output_text`
    /// als aktuellen Plantext und setzen den `PlanStale`-Rundenzähler
    /// zurück.
    ///
    /// Für `fs.write`/`fs.edit`/`fs.patch` wird geprüft, ob der Dateiname des
    /// `path`-Arguments im zuletzt bekannten Plantext vorkommt; falls ein
    /// Plantext existiert und der Dateiname dort nicht vorkommt, wird — pro
    /// Pfad höchstens einmal je Turn — [`DriftKind::PlanScopeDrift`]
    /// gemeldet.
    ///
    /// # Arguments
    /// - `tool_name` (`&str`): Name des ausgeführten Tools.
    /// - `arguments` (`&serde_json::Value`): vom Modell übergebene Argumente.
    /// - `status` (`ToolOutcomeStatus`): Erfolg oder Fehler des Aufrufs.
    /// - `output_text` (`&str`): Textform des Ergebnisses (Plantext bei
    ///   `plan.*`-Erfolg).
    ///
    /// # Returns
    /// [`GuardVerdict::Continue`], wenn `policy.enabled` falsch ist oder kein
    /// Befund vorliegt; sonst [`GuardVerdict::Warn`]/[`GuardVerdict::Abort`].
    pub fn observe_tool_result(
        &mut self,
        tool_name: &str,
        arguments: &serde_json::Value,
        status: ToolOutcomeStatus,
        output_text: &str,
    ) -> GuardVerdict {
        if !self.policy.enabled {
            return GuardVerdict::Continue;
        }

        let signature = call_signature(tool_name, arguments);

        match status {
            ToolOutcomeStatus::Error => {
                let count = self.failure_counts.entry(signature).or_insert(0);
                *count += 1;
                let count = *count;
                if count >= self.policy.repeated_failure_abort {
                    let event = self.event(
                        DriftKind::RepeatedFailingCall,
                        format!("`{tool_name}` ist {count}x mit derselben Signatur fehlgeschlagen"),
                        Some(tool_name),
                    );
                    return GuardVerdict::Abort {
                        hint: format!(
                            "[harw-Wächter] `{tool_name}` schlägt wiederholt mit denselben Argumenten fehl ({count}x) — Turn abgebrochen."
                        ),
                        event,
                    };
                }
                if count >= self.policy.repeated_failure_warn {
                    let event = self.event(
                        DriftKind::RepeatedFailingCall,
                        format!("`{tool_name}` ist {count}x mit derselben Signatur fehlgeschlagen"),
                        Some(tool_name),
                    );
                    return GuardVerdict::Warn {
                        hint: format!(
                            "[harw-Wächter] `{tool_name}` schlägt wiederholt mit denselben Argumenten fehl ({count}x) — andere Argumente oder Strategie erwägen."
                        ),
                        event,
                    };
                }
            }
            ToolOutcomeStatus::Success => {
                self.failure_counts.remove(&signature);
                self.seen_success_signatures.insert(signature);

                if let Some(rest) = tool_name.strip_prefix("plan.") {
                    let _ = rest;
                    self.last_plan_text = Some(output_text.to_owned());
                    self.rounds_since_last_plan_call = 0;
                    self.plan_stale_warned = false;
                }
            }
        }

        if PLAN_SCOPE_TOOLS.contains(&tool_name) {
            if let Some(plan_text) = self.last_plan_text.as_deref() {
                if let Some(path) = arguments.get("path").and_then(serde_json::Value::as_str) {
                    let file_name = file_name_of(path);
                    let already_warned = self.scope_drift_warned_paths.contains(path);
                    if !file_name.is_empty() && !plan_text.contains(file_name) && !already_warned {
                        self.scope_drift_warned_paths.insert(path.to_owned());
                        let event = self.event(
                            DriftKind::PlanScopeDrift,
                            format!("`{path}` kommt im zuletzt bekannten Plan nicht vor"),
                            Some(tool_name),
                        );
                        return GuardVerdict::Warn {
                            hint: format!(
                                "[harw-Wächter] `{path}` ist im aktuellen Plan nicht erwähnt — Planumfang prüfen."
                            ),
                            event,
                        };
                    }
                }
            }
        }

        GuardVerdict::Continue
    }

    /// Wertet das Ende einer Modellrunde aus.
    ///
    /// # Description
    /// `progressed` beschreibt, ob diese Runde laut Vertrag Fortschritt
    /// gemacht hat (mindestens ein erfolgreicher Tool-Aufruf mit neuer
    /// Signatur ODER Assistant-Text ohne Tool-Aufrufe — die Berechnung
    /// obliegt `harw-core/src/turn_loop.rs`). Ohne Fortschritt wird der
    /// Rundenzähler erhöht: ab `no_progress_rounds_warn` wird gewarnt, ab
    /// `no_progress_rounds_abort` abgebrochen. Zusätzlich wird — unabhängig
    /// von `progressed` — der Rundenzähler seit dem letzten `plan.*`-Aufruf
    /// erhöht; existiert ein Plantext und wird `plan_stale_rounds` erreicht,
    /// wird einmalig [`DriftKind::PlanStale`] gemeldet.
    ///
    /// # Arguments
    /// - `progressed` (`bool`): ob diese Runde Fortschritt machte.
    ///
    /// # Returns
    /// [`GuardVerdict::Continue`], wenn `policy.enabled` falsch ist oder kein
    /// Befund vorliegt; sonst [`GuardVerdict::Warn`]/[`GuardVerdict::Abort`].
    /// Ein `NoProgressRounds`-Befund hat Vorrang vor `PlanStale`.
    pub fn observe_round_end(&mut self, progressed: bool) -> GuardVerdict {
        if !self.policy.enabled {
            return GuardVerdict::Continue;
        }

        if progressed {
            self.rounds_without_progress = 0;
        } else {
            self.rounds_without_progress = self.rounds_without_progress.saturating_add(1);
        }
        self.rounds_since_last_plan_call = self.rounds_since_last_plan_call.saturating_add(1);

        if self.rounds_without_progress >= self.policy.no_progress_rounds_abort {
            let rounds = self.rounds_without_progress;
            let event = self.event(
                DriftKind::NoProgressRounds,
                format!("{rounds} Runden ohne Fortschritt"),
                None,
            );
            return GuardVerdict::Abort {
                hint: format!(
                    "[harw-Wächter] {rounds} Runden ohne erkennbaren Fortschritt — Turn abgebrochen."
                ),
                event,
            };
        }
        if self.rounds_without_progress >= self.policy.no_progress_rounds_warn {
            let rounds = self.rounds_without_progress;
            let event = self.event(
                DriftKind::NoProgressRounds,
                format!("{rounds} Runden ohne Fortschritt"),
                None,
            );
            return GuardVerdict::Warn {
                hint: format!(
                    "[harw-Wächter] {rounds} Runden mit wiederholten identischen Aufrufen ohne \
                     neues Ergebnis. Hinweis: Es gibt keinen Antwort-Cache — jeder Aufruf wurde \
                     echt ausgeführt. Beim Warten auf einen laufenden Prozess ist Wiederholen in \
                     Ordnung; sonst Vorgehen überdenken."
                ),
                event,
            };
        }

        if !self.plan_stale_warned
            && self.last_plan_text.is_some()
            && self.rounds_since_last_plan_call >= self.policy.plan_stale_rounds
        {
            self.plan_stale_warned = true;
            let rounds = self.rounds_since_last_plan_call;
            let event = self.event(
                DriftKind::PlanStale,
                format!("{rounds} Runden ohne `plan.*`-Aufruf"),
                None,
            );
            return GuardVerdict::Warn {
                hint: format!(
                    "[harw-Wächter] seit {rounds} Runden kein Plan-Update mehr — Plan noch aktuell?"
                ),
                event,
            };
        }

        GuardVerdict::Continue
    }
}

// ── Runde 7, Teil A2/A5: sitzungsweite Wächter ──────────────────────────────

/// Lese-Werkzeuge, die auf das Lesebudget eines Orchestrators zählen
/// (Runde 7, Teil A2). Dazu kommen alle Werkzeuge mit einem Präfix aus
/// [`ORCHESTRATOR_READ_TOOL_PREFIXES`].
pub const ORCHESTRATOR_READ_TOOLS: &[&str] = &[
    "fs.read",
    "fs.search",
    "fs.grep",
    "fs.glob",
    "fs.list",
    "lens.ask",
    "doc.read_pdf",
];

/// Präfixe von Lese-Werkzeugfamilien, die auf das Lesebudget zählen
/// (`explore.*`, `deps.source_*`).
pub const ORCHESTRATOR_READ_TOOL_PREFIXES: &[&str] = &["explore.", "deps.source_"];

/// Ablehnungstext, sobald ein Orchestrator sein Lesebudget überschreitet.
pub const READ_BUDGET_EXHAUSTED_MESSAGE: &str =
    "[harw-Wächter] Lesebudget erschöpft – delegiere an Explorer/Worker (`delegate_wave`)";

/// Name des Status-Werkzeugs für Hintergrund-Agenten (Runde 7, Teil A5).
pub const AGENT_STATUS_TOOL_NAME: &str = "agent.status";

/// Zeitfenster, in dem eine erneute `agent.status`-Abfrage derselben
/// `child_id` als Polling gilt (Runde 7, Teil A5).
pub const STATUS_POLL_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// Hinweis bei `agent.status`-Polling (Runde 7, Teil A5).
pub const STATUS_POLLING_HINT: &str = "[harw-Wächter] `agent.status` für denselben Agenten \
     innerhalb von 30 s erneut abgefragt – das Ergebnis kommt als Benachrichtigung. Nicht \
     abfragen, sondern auf die Benachrichtigung warten (oder die Nutzerin informieren und den \
     Turn beenden).";

/// Meldet, ob `tool_name` auf das Lesebudget eines Orchestrators zählt.
///
/// # Arguments
/// - `tool_name` (`&str`): der Werkzeugname.
///
/// # Returns
/// `true` für [`ORCHESTRATOR_READ_TOOLS`] und Werkzeuge mit einem Präfix aus
/// [`ORCHESTRATOR_READ_TOOL_PREFIXES`].
#[must_use]
pub fn is_orchestrator_read_tool(tool_name: &str) -> bool {
    ORCHESTRATOR_READ_TOOLS.contains(&tool_name)
        || ORCHESTRATOR_READ_TOOL_PREFIXES
            .iter()
            .any(|prefix| tool_name.starts_with(prefix))
}

/// Ergebnis einer sitzungsweiten Wächter-Prüfung **vor** der Ausführung.
#[derive(Debug)]
pub enum SessionGuardVerdict {
    /// Kein Befund; der Aufruf läuft normal.
    Continue,
    /// Der Aufruf läuft; `hint` wird an sein Ergebnis angehängt.
    Warn { event: DriftEvent, hint: String },
    /// Der Aufruf wird **nicht** ausgeführt; `message` ist sein
    /// Fehlerergebnis. Der Turn läuft weiter.
    Deny { event: DriftEvent, message: String },
}

/// Wächter-Zustand, der über **alle** Turns einer Sitzung lebt.
///
/// # Description
/// Runde 7, Teile A2/A5: [`TurnGuard`] wird pro Turn neu gebaut; das
/// Lesebudget eines Orchestrators und die Polling-Erkennung für
/// `agent.status` müssen aber über die ganze (Kind-)Sitzung zählen. Die
/// Sitzung besitzt deshalb genau einen `SessionGuardState`; der Turn-Loop
/// befragt ihn vor jeder Werkzeugausführung.
///
/// # Concurrency
/// Kein geteilter Zustand; lebt in der Sitzung und wird nur über `&mut`
/// verändert.
#[derive(Debug, Default)]
pub struct SessionGuardState {
    orchestrator_reads: u32,
    status_polls: HashMap<String, std::time::Instant>,
}

impl SessionGuardState {
    /// Bisher gezählte Lesezugriffe (einschließlich abgelehnter Versuche).
    #[must_use]
    pub fn orchestrator_reads(&self) -> u32 {
        self.orchestrator_reads
    }

    /// Zählt einen Lesezugriff eines Orchestrators und entscheidet über ihn.
    ///
    /// # Description
    /// Der Aufrufer stellt sicher, dass die Sitzung eine Orchestrator-Rolle
    /// hat. Nicht-Lese-Werkzeuge, ein abgeschalteter Wächter und
    /// `orchestrator_read_limit == 0` ergeben immer
    /// [`SessionGuardVerdict::Continue`], ohne zu zählen. Sonst gilt für den
    /// `n`-ten Lesezugriff: `n > limit` → [`SessionGuardVerdict::Deny`] mit
    /// [`READ_BUDGET_EXHAUSTED_MESSAGE`]; `n >= warn` →
    /// [`SessionGuardVerdict::Warn`]; sonst `Continue`.
    ///
    /// # Arguments
    /// - `policy` (`&GuardPolicy`): Schwellen der Sitzung.
    /// - `session_id` (`&harw_types::SessionId`): für das Drift-Ereignis.
    /// - `tool_name` (`&str`): das angefragte Werkzeug.
    ///
    /// # Returns
    /// Das Verdikt für diesen Aufruf.
    pub fn observe_orchestrator_read(
        &mut self,
        policy: &GuardPolicy,
        session_id: &harw_types::SessionId,
        tool_name: &str,
    ) -> SessionGuardVerdict {
        if !policy.enabled
            || policy.orchestrator_read_limit == 0
            || !is_orchestrator_read_tool(tool_name)
        {
            return SessionGuardVerdict::Continue;
        }
        self.orchestrator_reads = self.orchestrator_reads.saturating_add(1);
        let count = self.orchestrator_reads;
        let limit = policy.orchestrator_read_limit;
        let event = |detail: String| DriftEvent {
            kind: DriftKind::ReadBudget,
            session_id: session_id.to_string(),
            detail,
            tool_name: Some(tool_name.to_owned()),
            child_role: None,
        };
        if count > limit {
            return SessionGuardVerdict::Deny {
                event: event(format!(
                    "Lesezugriff {count} über dem Orchestrator-Lesebudget ({limit}) abgelehnt"
                )),
                message: format!(
                    "{READ_BUDGET_EXHAUSTED_MESSAGE}. Bereits {limit} eigene Lesezugriffe; \
                     Details liefern Explorer/Worker."
                ),
            };
        }
        if count >= policy.orchestrator_read_warn {
            return SessionGuardVerdict::Warn {
                event: event(format!("Lesezugriff {count} von {limit}")),
                hint: format!(
                    "[harw-Wächter] Lesezugriff {count} von {limit} – der eigene Überblick ist \
                     fast ausgeschöpft. Details an Explorer/Worker delegieren (`delegate_wave`)."
                ),
            };
        }
        SessionGuardVerdict::Continue
    }

    /// Erkennt `agent.status`-Polling (Runde 7, Teil A5).
    ///
    /// # Description
    /// Merkt sich je `child_id` (ohne Angabe: „alle") den Zeitpunkt der
    /// letzten Abfrage. Folgt eine weitere Abfrage derselben ID innerhalb von
    /// [`STATUS_POLL_WINDOW`], gibt es einen Hinweis — der Aufruf selbst läuft
    /// trotzdem. Andere Werkzeuge und ein abgeschalteter Wächter ergeben
    /// `Continue`.
    ///
    /// # Arguments
    /// - `policy` (`&GuardPolicy`): Schwellen der Sitzung.
    /// - `session_id` (`&harw_types::SessionId`): für das Drift-Ereignis.
    /// - `tool_name` (`&str`): das angefragte Werkzeug.
    /// - `arguments` (`&serde_json::Value`): seine Argumente.
    /// - `now` (`std::time::Instant`): der Prüfzeitpunkt.
    ///
    /// # Returns
    /// [`SessionGuardVerdict::Warn`] bei Polling, sonst `Continue`.
    pub fn observe_status_poll(
        &mut self,
        policy: &GuardPolicy,
        session_id: &harw_types::SessionId,
        tool_name: &str,
        arguments: &serde_json::Value,
        now: std::time::Instant,
    ) -> SessionGuardVerdict {
        if !policy.enabled || tool_name != AGENT_STATUS_TOOL_NAME {
            return SessionGuardVerdict::Continue;
        }
        let key = arguments
            .get("child_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty() && *id != "null")
            .unwrap_or("*")
            .to_owned();
        self.status_polls
            .retain(|_, seen| now.saturating_duration_since(*seen) < STATUS_POLL_WINDOW);
        let polled_recently = self.status_polls.contains_key(&key);
        self.status_polls.insert(key.clone(), now);
        if !polled_recently {
            return SessionGuardVerdict::Continue;
        }
        SessionGuardVerdict::Warn {
            event: DriftEvent {
                kind: DriftKind::StatusPolling,
                session_id: session_id.to_string(),
                detail: format!("`agent.status` für `{key}` binnen 30 s erneut abgefragt"),
                tool_name: Some(tool_name.to_owned()),
                child_role: None,
            },
            hint: STATUS_POLLING_HINT.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn sid() -> harw_types::SessionId {
        harw_types::SessionId::new()
    }

    #[test]
    fn test_observe_tool_result_warns_then_aborts_on_repeated_failure() {
        let session_id = sid();
        let mut guard = TurnGuard::new(GuardPolicy::default(), &session_id);
        let args = serde_json::json!({"a": 1});

        assert!(matches!(
            guard.observe_tool_result("shell.run", &args, ToolOutcomeStatus::Error, "boom"),
            GuardVerdict::Continue
        ));
        assert!(matches!(
            guard.observe_tool_result("shell.run", &args, ToolOutcomeStatus::Error, "boom"),
            GuardVerdict::Warn { .. }
        ));
        assert!(matches!(
            guard.observe_tool_result("shell.run", &args, ToolOutcomeStatus::Error, "boom"),
            GuardVerdict::Abort { .. }
        ));
    }

    #[test]
    fn test_observe_tool_result_success_resets_failure_count() {
        let session_id = sid();
        let mut guard = TurnGuard::new(GuardPolicy::default(), &session_id);
        let args = serde_json::json!({"a": 1});

        let _ = guard.observe_tool_result("shell.run", &args, ToolOutcomeStatus::Error, "boom");
        assert!(matches!(
            guard.observe_tool_result("shell.run", &args, ToolOutcomeStatus::Success, "ok"),
            GuardVerdict::Continue
        ));
        assert!(matches!(
            guard.observe_tool_result("shell.run", &args, ToolOutcomeStatus::Error, "boom"),
            GuardVerdict::Continue
        ));
    }

    #[test]
    fn test_observe_round_end_no_progress_warns_then_aborts() {
        let session_id = sid();
        let policy = GuardPolicy {
            no_progress_rounds_warn: 2,
            no_progress_rounds_abort: 3,
            ..GuardPolicy::default()
        };
        let mut guard = TurnGuard::new(policy, &session_id);

        assert!(matches!(
            guard.observe_round_end(false),
            GuardVerdict::Continue
        ));
        assert!(matches!(
            guard.observe_round_end(false),
            GuardVerdict::Warn { .. }
        ));
        assert!(matches!(
            guard.observe_round_end(false),
            GuardVerdict::Abort { .. }
        ));
    }

    #[test]
    fn test_observe_round_end_progress_resets_counter() {
        let session_id = sid();
        let policy = GuardPolicy {
            no_progress_rounds_warn: 1,
            no_progress_rounds_abort: 5,
            ..GuardPolicy::default()
        };
        let mut guard = TurnGuard::new(policy, &session_id);

        assert!(matches!(
            guard.observe_round_end(false),
            GuardVerdict::Warn { .. }
        ));
        assert!(matches!(
            guard.observe_round_end(true),
            GuardVerdict::Continue
        ));
    }

    // Fix C (Moduldoku „Achter Nachtrag"): das Modell hatte den alten Hinweis
    // "... Strategie überdenken." als Beleg für einen nicht existenten
    // Antwort-Cache fehlgedeutet und begann, Befehle künstlich zu variieren
    // (`echo LAEUFT-v2`, `-v3`), obwohl es legitim auf einen laufenden
    // Hintergrundprozess wartete. Der neue Hinweistext muss explizit
    // klarstellen, dass es keinen Cache gibt.
    #[test]
    fn test_observe_round_end_no_progress_hint_denies_response_cache() -> TestResult {
        let session_id = sid();
        let policy = GuardPolicy {
            no_progress_rounds_warn: 1,
            no_progress_rounds_abort: 100,
            ..GuardPolicy::default()
        };
        let mut guard = TurnGuard::new(policy, &session_id);

        match guard.observe_round_end(false) {
            GuardVerdict::Warn { hint, .. } => {
                assert!(
                    hint.contains("kein"),
                    "hint must explicitly deny a response cache: {hint}"
                );
                assert!(
                    hint.contains("Cache"),
                    "hint must name the misunderstood concept ('Cache') directly: {hint}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected GuardVerdict::Warn, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_observe_round_end_plan_stale_warns_once() {
        let session_id = sid();
        let policy = GuardPolicy {
            no_progress_rounds_warn: 100,
            no_progress_rounds_abort: 200,
            plan_stale_rounds: 2,
            ..GuardPolicy::default()
        };
        let mut guard = TurnGuard::new(policy, &session_id);
        let args = serde_json::json!({});
        let _ = guard.observe_tool_result(
            "plan.update",
            &args,
            ToolOutcomeStatus::Success,
            "plan body mentions foo.rs",
        );

        assert!(matches!(
            guard.observe_round_end(true),
            GuardVerdict::Continue
        ));
        assert!(matches!(
            guard.observe_round_end(true),
            GuardVerdict::Warn { .. }
        ));
        // Einmalig: ein weiterer Aufruf ohne neuen Plan-Call warnt nicht erneut.
        assert!(matches!(
            guard.observe_round_end(true),
            GuardVerdict::Continue
        ));
    }

    #[test]
    fn test_observe_tool_result_plan_scope_drift_warns_once_per_path() {
        let session_id = sid();
        let mut guard = TurnGuard::new(GuardPolicy::default(), &session_id);
        let plan_args = serde_json::json!({});
        let _ = guard.observe_tool_result(
            "plan.update",
            &plan_args,
            ToolOutcomeStatus::Success,
            "touch only foo.rs",
        );

        let write_args = serde_json::json!({"path": "src/bar.rs"});
        assert!(matches!(
            guard.observe_tool_result(
                "fs.write",
                &write_args,
                ToolOutcomeStatus::Success,
                "written",
            ),
            GuardVerdict::Warn { .. }
        ));
        assert!(matches!(
            guard.observe_tool_result(
                "fs.write",
                &write_args,
                ToolOutcomeStatus::Success,
                "written",
            ),
            GuardVerdict::Continue
        ));
    }

    #[test]
    fn test_observe_tool_result_plan_scope_drift_skips_path_mentioned_in_plan() {
        let session_id = sid();
        let mut guard = TurnGuard::new(GuardPolicy::default(), &session_id);
        let plan_args = serde_json::json!({});
        let _ = guard.observe_tool_result(
            "plan.update",
            &plan_args,
            ToolOutcomeStatus::Success,
            "touch bar.rs and foo.rs",
        );

        let write_args = serde_json::json!({"path": "src/bar.rs"});
        assert!(matches!(
            guard.observe_tool_result(
                "fs.write",
                &write_args,
                ToolOutcomeStatus::Success,
                "written",
            ),
            GuardVerdict::Continue
        ));
    }

    #[test]
    fn test_disabled_policy_never_reports() {
        let session_id = sid();
        let policy = GuardPolicy {
            enabled: false,
            ..GuardPolicy::default()
        };
        let mut guard = TurnGuard::new(policy, &session_id);
        let args = serde_json::json!({});
        for _ in 0..10 {
            assert!(matches!(
                guard.observe_tool_result("shell.run", &args, ToolOutcomeStatus::Error, "boom"),
                GuardVerdict::Continue
            ));
            assert!(matches!(
                guard.observe_round_end(false),
                GuardVerdict::Continue
            ));
        }
    }

    #[test]
    fn test_drift_kind_key_matches_serde_name() -> TestResult {
        assert_eq!(
            DriftKind::RepeatedFailingCall.key(),
            "repeated_failing_call"
        );
        assert_eq!(DriftKind::ChildLeaseExpired.key(), "child_lease_expired");
        let value = serde_json::to_value(DriftKind::PlanScopeDrift)
            .map_err(ctx("DriftKind::PlanScopeDrift serialisieren"))?;
        assert_eq!(value, serde_json::json!("plan_scope_drift"));
        Ok(())
    }

    // --- Runde 7, Teile A2/A5 ------------------------------------------

    #[test]
    fn the_sixth_orchestrator_read_is_denied_after_a_warning_from_the_fourth() -> TestResult {
        let session_id = sid();
        let policy = GuardPolicy::default();
        let mut state = SessionGuardState::default();
        let tools = [
            "fs.read",
            "fs.list",
            "explore.tree",
            "fs.grep",
            "deps.source_read",
            "fs.read",
        ];
        let verdicts: Vec<SessionGuardVerdict> = tools
            .iter()
            .map(|tool| state.observe_orchestrator_read(&policy, &session_id, tool))
            .collect();
        assert!(matches!(verdicts[0], SessionGuardVerdict::Continue));
        assert!(matches!(verdicts[2], SessionGuardVerdict::Continue));
        assert!(matches!(verdicts[3], SessionGuardVerdict::Warn { .. }));
        assert!(matches!(verdicts[4], SessionGuardVerdict::Warn { .. }));
        match &verdicts[5] {
            SessionGuardVerdict::Deny { event, message } => {
                assert_eq!(event.kind, DriftKind::ReadBudget);
                assert!(
                    message.starts_with(READ_BUDGET_EXHAUSTED_MESSAGE),
                    "{message}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "the sixth read must be denied, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn non_read_tools_and_a_zero_limit_do_not_touch_the_read_budget() {
        let session_id = sid();
        let mut state = SessionGuardState::default();
        for _ in 0..10 {
            assert!(matches!(
                state.observe_orchestrator_read(
                    &GuardPolicy::default(),
                    &session_id,
                    "delegate_wave"
                ),
                SessionGuardVerdict::Continue
            ));
        }
        assert_eq!(state.orchestrator_reads(), 0);
        let off = GuardPolicy {
            orchestrator_read_limit: 0,
            ..GuardPolicy::default()
        };
        for _ in 0..10 {
            assert!(matches!(
                state.observe_orchestrator_read(&off, &session_id, "fs.read"),
                SessionGuardVerdict::Continue
            ));
        }
    }

    #[test]
    fn read_tool_classification_covers_the_families() {
        for tool in [
            "fs.read",
            "fs.search",
            "fs.grep",
            "fs.glob",
            "fs.list",
            "explore.find",
            "lens.ask",
            "deps.source_search",
            "doc.read_pdf",
        ] {
            assert!(is_orchestrator_read_tool(tool), "{tool}");
        }
        for tool in ["fs.write", "delegate_wave", "agent.status", "shell.exec"] {
            assert!(!is_orchestrator_read_tool(tool), "{tool}");
        }
    }

    #[test]
    fn agent_status_polling_on_the_same_child_warns_within_thirty_seconds() -> TestResult {
        let session_id = sid();
        let policy = GuardPolicy::default();
        let mut state = SessionGuardState::default();
        let start = std::time::Instant::now();
        let args = serde_json::json!({ "child_id": "kind-1" });
        assert!(matches!(
            state.observe_status_poll(&policy, &session_id, "agent.status", &args, start),
            SessionGuardVerdict::Continue
        ));
        let other = serde_json::json!({ "child_id": "kind-2" });
        assert!(matches!(
            state.observe_status_poll(
                &policy,
                &session_id,
                "agent.status",
                &other,
                start + std::time::Duration::from_secs(1)
            ),
            SessionGuardVerdict::Continue
        ));
        match state.observe_status_poll(
            &policy,
            &session_id,
            "agent.status",
            &args,
            start + std::time::Duration::from_secs(5),
        ) {
            SessionGuardVerdict::Warn { event, hint } => {
                assert_eq!(event.kind, DriftKind::StatusPolling);
                assert!(hint.contains("Benachrichtigung"), "{hint}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "polling must warn, got {other:?}"
                )));
            }
        }
        assert!(matches!(
            state.observe_status_poll(
                &policy,
                &session_id,
                "agent.status",
                &args,
                start + std::time::Duration::from_secs(60)
            ),
            SessionGuardVerdict::Continue
        ));
        Ok(())
    }

    #[test]
    fn new_drift_kinds_have_stable_keys() {
        assert_eq!(DriftKind::ReadBudget.key(), "read_budget");
        assert_eq!(DriftKind::StatusPolling.key(), "status_polling");
    }

    /// Advisor, der nur `SessionResolvedSet` nutzt (Muster des Runtime-Advisors).
    struct SetAdvisor(SessionResolvedSet);

    impl PitfallAdvisor for SetAdvisor {
        fn advise(&self, _tool: &str, _args: &serde_json::Value) -> Option<String> {
            Some("hint".to_owned())
        }
        fn advise_in_session(
            &self,
            session_id: &harw_types::SessionId,
            _tool: &str,
            _args: &serde_json::Value,
        ) -> Option<String> {
            (!self.0.contains(session_id, "p")).then(|| "hint".to_owned())
        }
        fn resolved_in_session(
            &self,
            session_id: &harw_types::SessionId,
            _tool: &str,
            _args: &serde_json::Value,
        ) {
            self.0.insert(session_id, "p");
        }
    }

    #[test]
    fn test_resolution_in_session_a_does_not_hide_the_hint_in_session_b() {
        let advisor = SetAdvisor(SessionResolvedSet::new());
        let (a, b) = (sid(), sid());
        let args = serde_json::json!({});
        assert!(advisor.advise_in_session(&a, "t", &args).is_some());
        advisor.resolved_in_session(&a, "t", &args);
        assert!(advisor.advise_in_session(&a, "t", &args).is_none());
        assert!(
            advisor.advise_in_session(&b, "t", &args).is_some(),
            "session B must still see the pitfall"
        );
    }

    #[test]
    fn test_default_resolved_in_session_is_a_noop_and_never_goes_global() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Legacy(AtomicUsize);
        impl PitfallAdvisor for Legacy {
            fn advise(&self, _t: &str, _a: &serde_json::Value) -> Option<String> {
                None
            }
            fn resolved(&self, _t: &str, _a: &serde_json::Value) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let legacy = Legacy(AtomicUsize::new(0));
        legacy.resolved_in_session(&sid(), "t", &serde_json::json!({}));
        assert_eq!(legacy.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_session_resolved_set_is_bounded_and_evicts_oldest() {
        let set = SessionResolvedSet::with_limits(2, 2);
        let (a, b, c) = (sid(), sid(), sid());
        set.insert(&a, "x");
        set.insert(&b, "x");
        set.insert(&c, "x");
        assert!(!set.contains(&a, "x"), "oldest session evicted");
        assert!(set.contains(&b, "x") && set.contains(&c, "x"));
        set.insert(&c, "y");
        set.insert(&c, "z");
        assert!(!set.contains(&c, "x"), "oldest entry evicted");
        assert!(set.contains(&c, "y") && set.contains(&c, "z"));
        set.forget(&c);
        assert!(!set.contains(&c, "z"));
    }
}
