//! Capabilities — was eine Extension DARF.

use harw_authority::SandboxSpec;
use harw_catalog::AgentSuggestions;
use harw_context::ContextCeiling;
use harw_types::{SessionId, ToolCallId};
use jiff::Timestamp;
use std::future::Future;
use std::pin::Pin;

pub type SpawnFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SessionId, AgentSpawnError>> + Send + 'a>>;

/// Handoff: Sub-Agent starten.
pub trait AgentSpawner: Send + Sync {
    /// Spawn a governed child with immutable parent-derived context.
    ///
    /// There is intentionally no context-free fallback: a spawner that does
    /// not receive this authority cannot prove that the child has inherited
    /// its parent's workspace and permissions. Implementations must validate
    /// the role and apply the sandbox before allocating a child session.
    fn spawn_child<'a>(
        &'a self,
        role: &'a str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a>;

    /// The core calls this only after it has correlated a child result to the
    /// parent handoff. Managed spawners use it to release concurrency slots.
    /// A no-op default keeps isolated test/dummy spawners lightweight; it does
    /// not grant any authority.
    fn child_finished(&self, _child: &SessionId) {}

    /// Notify the spawner that a terminal child result has been durably written
    /// to the parent, including the result's completion timestamp. The core
    /// invokes this only after that durable write succeeds. Implementations
    /// must surface failures so the child lease is preserved for recovery.
    ///
    /// The default keeps legacy spawners compatible by delegating to
    /// [`Self::child_finished`] and reporting success.
    fn child_completed(
        &self,
        child: &SessionId,
        _completed_at: Timestamp,
    ) -> Result<(), AgentSpawnError> {
        self.child_finished(child);
        Ok(())
    }

    /// Sichtbare Delegationsziele für `parent_session_id` (Addendum F+G,
    /// Nachtrag F — Delegationsprojektion).
    ///
    /// # Description
    /// Namen registrierter Rollen, an die `parent_session_id` laut
    /// derselben zwei Prädikaten delegieren dürfte, die auch die echte
    /// Admission durchsetzt (`can_delegate_to` /
    /// `harw_core::delegation_visibility::visible_delegation_targets`). Ein
    /// Aufrufer (`harw-core/src/turn_loop.rs`) hängt daraus — nur wenn die
    /// Liste nicht leer ist — einen deterministischen Kontextblock an die
    /// Modellanfrage an.
    ///
    /// Der Default liefert immer eine leere Liste, damit bestehende
    /// [`AgentSpawner`]-Implementierungen (Tests, Dummy-Spawner) ohne
    /// Änderung weiter kompilieren; sie zeigen dann schlicht keine
    /// Delegationsziele an.
    ///
    /// # Arguments
    /// - `parent_session_id` (`&SessionId`): der delegieren wollende Agent.
    ///
    /// # Returns
    /// Exakte, sortierte registrierte Rollennamen; leer, wenn nichts
    /// sichtbar ist oder die Implementierung diese Projektion nicht anbietet.
    fn delegation_target_names(&self, _parent_session_id: &SessionId) -> Vec<String> {
        Vec::new()
    }

    /// Runde 5, Teil K: `true`, wenn `child` abgekoppelt im Hintergrund
    /// weiterläuft, obwohl sein Elternteil das Handoff-Ergebnis schon
    /// bekommen hat. Der Kern meldet dann **kein** `ChildCompleted` beim
    /// Wiederaufnehmen des Elternteils (das Kind ist nicht fertig); der
    /// Hintergrund-Treiber meldet es beim echten Ende. Der Default `false`
    /// lässt jede andere Implementierung unverändert.
    fn child_runs_in_background(&self, _child: &SessionId) -> bool {
        false
    }

    /// Runde 9, E3: die Rolle, mit der `caller` sein eigenes, bereits
    /// beendetes Kind `child_id` fortsetzen kann (`continue_from`). Der Kern
    /// macht daraus bei `agent.message` an ein beendetes Kind eine
    /// Fortsetzung mit der Nachricht als Auftrag. `None` für fremde,
    /// unbekannte, laufende oder nicht fortsetzbare Kinder; der Default
    /// `None` lässt jede andere Implementierung unverändert.
    fn resumable_child_role(&self, _caller: &SessionId, _child_id: &str) -> Option<String> {
        None
    }

    /// Plan R9, Teil C/E1: die sichtbaren Delegationsziele von
    /// `parent_session_id` samt Katalogdaten, gefiltert nach Plan-Modus.
    ///
    /// # Description
    /// Dieselbe Sichtbarkeit wie [`Self::delegation_target_names`], zusätzlich
    /// die Plan-Modus-Regel: im Plan-Modus bleiben nur lesende Ziele
    /// delegierbar; die übrigen sichtbaren Ziele stehen in
    /// [`DelegationTargets::withheld_by_plan_mode`] (nur für eine benannte
    /// Ablehnung, nie als Angebot). `plan_mode` ist der Modus, den der
    /// Aufrufer selbst kennt (die Sitzung der Wurzel); eine Implementierung
    /// darf zusätzlich einen geerbten Plan-Modus berücksichtigen.
    ///
    /// Der Default kennt weder Lese-Eigenschaft noch Katalogdaten: außerhalb
    /// des Plan-Modus liefert er die Namen aus
    /// [`Self::delegation_target_names`], im Plan-Modus kein Ziel (das
    /// Verhalten vor Plan R9).
    ///
    /// # Errors
    /// [`DelegationUnavailable`] mit benanntem Grund (Resttiefe 0, fehlender
    /// Spawn-Kontext) — nie ein Katalog-Orakel: der Grund betrifft allein den
    /// Aufrufer.
    fn delegation_targets(
        &self,
        parent_session_id: &SessionId,
        plan_mode: bool,
    ) -> Result<DelegationTargets, DelegationUnavailable> {
        let names = self.delegation_target_names(parent_session_id);
        let infos = names.into_iter().map(DelegationTargetInfo::named).collect();
        Ok(if plan_mode {
            DelegationTargets {
                targets: Vec::new(),
                plan_mode,
                withheld_by_plan_mode: infos,
            }
        } else {
            DelegationTargets {
                targets: infos,
                plan_mode,
                withheld_by_plan_mode: Vec::new(),
            }
        })
    }

    /// Plan R9, E1: der Aufrufer meldet seinen aktuellen Interaktionsmodus
    /// (`plan_mode = true` im Plan-Modus), damit eine Implementierung ihn an
    /// Kinder vererben kann, deren Elternteil sie sonst nicht beobachtet (die
    /// extern gefahrene Wurzelsitzung). Der Default ignoriert die Meldung.
    fn note_caller_mode(&self, _caller: &SessionId, _plan_mode: bool) {}
}

/// Katalogdaten eines sichtbaren Delegationsziels (Plan R9, Teil C).
///
/// # Beschreibung
/// Reine Daten für `agents.catalog`, die Beschreibungen der
/// `transfer_to_*`-Werkzeuge und die Plan-Modus-Regel. Sie verleihen keine
/// Rechte; die Admission prüft jedes Ziel unabhängig davon.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DelegationTargetInfo {
    /// Exakter Spawn-Name.
    pub name: String,
    /// Organisationsrolle als Label (`worker`, `child-orchestrator`, …).
    pub role: String,
    /// Einzeilige Beschreibung, falls bekannt.
    pub description: Option<String>,
    /// Fest gebundene Skills der Definition.
    pub skills: Vec<String>,
    /// Einzeilige Rechte-Zusammenfassung (Profil, lesend/schreibend, Werkzeuge).
    pub profile_summary: String,
    /// `true`, wenn das Ziel weder schreibt noch Prozesse startet.
    pub read_only: bool,
    /// Token-Budget des Ziels, falls es eines trägt.
    pub budget_tokens: Option<u64>,
    /// `true` für einen benutzerdefinierten Agenten, `false` für eine
    /// eingebaute Rolle.
    pub custom: bool,
}

impl DelegationTargetInfo {
    /// Ein Ziel, von dem nur der Name bekannt ist (lesend: unbekannt, also
    /// `false` — fail-closed für den Plan-Modus).
    #[must_use]
    pub fn named(name: String) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }
}

/// Die sichtbaren Delegationsziele eines Aufrufers (Plan R9, Teil C/E1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DelegationTargets {
    /// Die delegierbaren Ziele, nach Name sortiert (Cache-stabil).
    pub targets: Vec<DelegationTargetInfo>,
    /// `true`, wenn der Aufrufer (selbst oder geerbt) im Plan-Modus ist.
    pub plan_mode: bool,
    /// Sichtbare Ziele, die allein der Plan-Modus zurückhält (schreibend oder
    /// ausführend), nach Name sortiert. Nur für die benannte Ablehnung.
    pub withheld_by_plan_mode: Vec<DelegationTargetInfo>,
}

impl DelegationTargets {
    /// Die Namen der delegierbaren Ziele in Katalogreihenfolge.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(|target| target.name.clone())
            .collect()
    }

    /// Das delegierbare Ziel `name`, falls vorhanden.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&DelegationTargetInfo> {
        self.targets.iter().find(|target| target.name == name)
    }

    /// `true`, wenn `name` sichtbar ist, aber allein der Plan-Modus es
    /// zurückhält.
    #[must_use]
    pub fn is_withheld_by_plan_mode(&self, name: &str) -> bool {
        self.withheld_by_plan_mode
            .iter()
            .any(|target| target.name == name)
    }
}

/// Warum ein Aufrufer gerade gar nicht delegieren kann (Plan R9, Teil C).
///
/// # Beschreibung
/// Die Meldungen nennen nur den Zustand des Aufrufers selbst, nie ein Ziel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationUnavailable {
    /// Die Spawn-Tiefe des Aufrufers ist ausgeschöpft.
    DepthExhausted,
    /// Für den Aufrufer liegt kein vertrauenswürdiger Spawn-Kontext vor
    /// (interner Fehler: unbekannte Sitzung, fehlender Kontext, Sperre).
    NoSpawnContext {
        /// Technisches Detail für Log und Meldung.
        detail: String,
    },
}

impl std::fmt::Display for DelegationUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DepthExhausted => write!(
                f,
                "Restliche Spawn-Tiefe 0: du darfst keine weiteren Agenten starten"
            ),
            Self::NoSpawnContext { detail } => {
                write!(f, "Kein Spawn-Kontext (interner Fehler): {detail}")
            }
        }
    }
}

impl std::error::Error for DelegationUnavailable {}

#[derive(Debug, Clone)]
pub struct SpawnInput {
    /// Trusted parent correlation established by the orchestrator. This is
    /// not model-generated child context and must be validated by a managed
    /// spawner before it creates a session.
    pub parent_session_id: SessionId,
    /// Exact parent handoff call that owns this child. It is established by
    /// the core before the spawner runs and enables zombie reconciliation to
    /// deliver an error to the correct pending handoff.
    pub handoff_call_id: ToolCallId,
    pub instructions: Option<String>,
    pub context: serde_json::Value,
    /// The context ceiling the child declares wanting, carried directly
    /// alongside the rest of this handoff — not layered on in a later,
    /// separate step.
    ///
    /// # Warum hier, nicht später
    /// A managed spawner (`harw_core::child_controller::ManagedAgentSpawner::admit`)
    /// cuts this field against the parent's own ceiling in the very same
    /// admission call that derives the child's sandbox from the parent's
    /// sandbox. If the cut happened one step later — a separate function a
    /// future caller could forget to invoke — there would be a window in
    /// which a child carries a context program whose ceiling nobody has
    /// enforced yet. That window is a silent authority gap, not a crash, and
    /// gaps like that stay undiscovered far longer than a panic would.
    ///
    /// # `None` vs. `Some`
    /// `None` means the child requests no ceiling of its own; it simply
    /// inherits whatever ceiling its parent already enforces, unchanged.
    /// This is always safe — inheriting the parent's already-cut ceiling can
    /// never widen anything. `Some(ceiling)` is a declared demand that the
    /// admitting spawner must verify is already fully contained within the
    /// parent's ceiling. A demand that is not contained must cause admission
    /// to fail closed with an error naming the exceeded aspect — it must
    /// never be silently narrowed to fit, because a silently pruned demand
    /// leaves the child running with less context than its own declaration
    /// promised, and nobody is told. That is the same failure mode as a
    /// silently dropped `must_include`.
    pub ceiling: Option<ContextCeiling>,
}

#[derive(Debug)]
pub struct AgentSpawnError {
    pub message: String,
}

impl std::fmt::Display for AgentSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "agent spawn failed: {}", self.message)
    }
}
impl std::error::Error for AgentSpawnError {}

/// Ein laufendes Kind hat eine Budget-Dimension erschöpft.
///
/// # Beschreibung
/// Laufzeitfehler eines bereits gestarteten Kindes — ausdrücklich **kein**
/// Spawn-Fehler: die Meldung beginnt deshalb nicht mit „agent spawn failed",
/// sondern bleibt beim maschinenlesbaren Format
/// `budget_exceeded: <dimension> (limit=<n>, used=<m>)`. Das Token-Budget
/// erzeugt diesen Fehler nicht mehr: dort liefert der Spawner ein
/// Teilergebnis mit `budget_exhausted = true` (siehe
/// `harw_core::child_controller::ChildRunResult`). Übrig bleiben Wanduhr und
/// Werkzeugaufrufe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildBudgetExhausted {
    /// Die erschöpfte Dimension (`"tokens"`, `"tool_calls"`, `"wall_time"`).
    pub dimension: String,
    /// Das Limit in der Einheit der Dimension.
    pub limit: u64,
    /// Der gemessene Verbrauch in derselben Einheit.
    pub used: u64,
}

impl std::fmt::Display for ChildBudgetExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "budget_exceeded: {} (limit={}, used={})",
            self.dimension, self.limit, self.used
        )
    }
}
impl std::error::Error for ChildBudgetExhausted {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct LegacySpawner {
        finished: AtomicUsize,
    }

    impl AgentSpawner for LegacySpawner {
        fn spawn_child<'a>(
            &'a self,
            _role: &'a str,
            _input: SpawnInput,
            _sandbox: SandboxSpec,
            _suggestions: Option<AgentSuggestions>,
        ) -> SpawnFuture<'a> {
            Box::pin(async { Ok(SessionId::new()) })
        }

        fn child_finished(&self, _child: &SessionId) {
            self.finished.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn legacy_spawner_default_child_completed_calls_child_finished() -> TestResult {
        let spawner = LegacySpawner {
            finished: AtomicUsize::new(0),
        };

        AgentSpawner::child_completed(&spawner, &SessionId::new(), Timestamp::now())
            .map_err(ctx("legacy completion hook should report success"))?;

        assert_eq!(spawner.finished.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
