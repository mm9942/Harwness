//! TUI-side implementation of the `SessionController` service.
//!
//! # Verantwortungsbereich
//! Ops mutieren Session-Zustand via [`SessionController::set_*`]. Die TUI hält einen
//! `Arc<TuiSessionController>` und legt ihn in jede `OpContext.services`. Zwischen
//! Turns wird [`TuiSessionController::apply_to_session`] aufgerufen, um ausstehende
//! Mutationen auf die lebende [`AgentSession`] anzuwenden.
//!
//! # Schlüsseltypen
//! - [`TuiSessionController`] — konkrete [`SessionController`]-Implementierung für die TUI
//! - [`SharedTuiSessionController`] — `Arc<TuiSessionController>` für die ServiceMap
//!
//! # Nebenläufigkeit
//! [`TuiSessionController`] ist `Send + Sync` via `Mutex<Inner>`. Arc-geklonte Handles
//! teilen denselben Zustand über Thread-Grenzen hinweg sicher.
//!
//! # Fehlertypen
//! [`harw_operations::SessionControlError`] — insbesondere `Disconnected` bei vergiftetem Mutex.
//!
//! # Spec
//! harw-tui Design §session_controller — TUI-seitige Implementierung.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_tui::session_controller::TuiSessionController;
//! use harw_operations::SessionController;
//! use harw_types::ReasoningEffort;
//!
//! let ctrl = TuiSessionController::new();
//! ctrl.set_reasoning_effort(Some(ReasoningEffort::High)).unwrap();
//! let snap = ctrl.snapshot();
//! assert_eq!(snap.reasoning_effort, Some(ReasoningEffort::High));
//! ```

use std::sync::{Arc, Mutex};

use harw_core::{AgentSession, InteractionMode};
use harw_operations::{SessionControlError, SessionControlSnapshot, SessionController};
use harw_types::{ModelId, ProviderId, ReasoningEffort};

// ── Inner ────────────────────────────────────────────────────────────────────

/// Interner, lock-geschützter Zustand des [`TuiSessionController`].
///
/// # Beschreibung
/// Hält die ausstehenden Session-Mutationen sowie zwei Generationszähler, über
/// die [`TuiSessionController::apply_to_session`] effizient entscheiden kann,
/// ob es etwas zu drainieren gibt.
///
/// # Spec
/// harw-tui Design §session_controller — Inner-Zustand.
#[derive(Debug, Default)]
struct Inner {
    /// Ausstehender Reasoning-Effort; `None` = Provider-Default.
    reasoning_effort: Option<ReasoningEffort>,
    /// Aktiv gewählte Modell-ID (in Controller gespeichert; wird bei
    /// einem späteren Provider-Routing-Wave effektiv).
    active_model: Option<String>,
    /// Aktiv gewählter Provider (in Controller gespeichert; wie `active_model`).
    active_provider: Option<String>,
    /// Ausstehender Interaktionsmodus; `None` = nie einer angefordert, die
    /// Session behält ihren eigenen (`InteractionMode::Chat` als Default).
    ///
    /// Hier **typisiert** gespeichert, obwohl der Trait `&str` spricht: der
    /// Trait liegt in `harw-operations` unterhalb von `harw-core`, die TUI
    /// hingegen kennt die Laufzeit. Der Name wird genau einmal geparst — in
    /// [`SessionController::request_mode`] — und ein unbekannter Modus stirbt
    /// dort, nicht erst beim Anwenden.
    interaction_mode: Option<InteractionMode>,
    /// Mutations-Generation: wird bei jedem Setter-Aufruf inkrementiert.
    generation: u64,
    /// Letzte erfolgreich auf eine Session angewendete Generation.
    applied_generation: u64,
}

// ── TuiSessionController ─────────────────────────────────────────────────────

/// Konkrete [`SessionController`]-Implementierung für den TUI-Laufzeit-Pfad.
///
/// # Beschreibung
/// Alle Setter schreiben in ein `Mutex<Inner>` und erhöhen einen Generationszähler.
/// [`Self::apply_to_session`] prüft vor dem Drain, ob die Generationen auseinanderlaufen,
/// und ist damit ein No-op wenn keine Mutation ansteht.
///
/// [`Self::apply_to_session`] überträgt `reasoning_effort`, `active_model` und
/// `active_provider` vollständig auf die [`AgentSession`] am Turn-Boundary.
///
/// # Nebenläufigkeit
/// `Send + Sync` — intern via `std::sync::Mutex` geschützt. `Arc::clone` eines
/// Handles teilt denselben Zustand ohne Kopie der Innenwerte.
///
/// # Fehlertypen
/// Setter geben [`SessionControlError::Disconnected`] zurück, wenn der Mutex
/// vergiftet ist (Panic in einem anderen Thread).
///
/// # Spec
/// harw-tui Design §session_controller — TuiSessionController.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_tui::session_controller::TuiSessionController;
/// use harw_operations::SessionController;
/// use harw_types::ReasoningEffort;
///
/// let ctrl = TuiSessionController::new();
/// ctrl.set_reasoning_effort(Some(ReasoningEffort::Medium)).unwrap();
/// assert_eq!(ctrl.snapshot().reasoning_effort, Some(ReasoningEffort::Medium));
/// ```
pub struct TuiSessionController {
    inner: Mutex<Inner>,
}

impl TuiSessionController {
    /// Erzeugt einen neuen `TuiSessionController` mit leerem Zustand.
    ///
    /// # Rückgabe
    /// Ein neuer [`TuiSessionController`]; alle Snapshot-Felder `None`.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_tui::session_controller::TuiSessionController;
    /// let ctrl = TuiSessionController::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Erzeugt einen Controller, der mit einem Snapshot vorbelegt ist (für Tests).
    ///
    /// # Argumente
    /// - `snap` ([`SessionControlSnapshot`]): Initialzustand; Generationszähler
    ///   bleiben auf 0, so dass kein automatischer Apply ausgelöst wird.
    ///
    /// # Rückgabe
    /// Ein vorbelegt neuer [`TuiSessionController`].
    ///
    /// # Spec
    /// harw-tui Design §session_controller — with_snapshot.
    #[must_use]
    pub fn with_snapshot(snap: SessionControlSnapshot) -> Self {
        let inner = Inner {
            reasoning_effort: snap.reasoning_effort,
            active_model: snap.active_model,
            active_provider: snap.active_provider,
            // Ein unbekannter Modusname im Snapshot wird verworfen statt zu
            // raten — `parse` ist die einzige Stelle, die Namen anerkennt.
            interaction_mode: snap
                .interaction_mode
                .as_deref()
                .and_then(InteractionMode::parse),
            generation: 0,
            applied_generation: 0,
        };
        Self {
            inner: Mutex::new(inner),
        }
    }

    /// Draint ausstehende Mutationen und wendet sie auf `session` an.
    ///
    /// # Beschreibung
    /// Prüft, ob `generation > applied_generation`. Bei Gleichheit ist dies ein
    /// billiger No-op (kein Lock-Contention). Andernfalls werden die ausstehenden
    /// Werte auf die [`AgentSession`] geschrieben und `applied_generation` nachgezogen.
    ///
    /// Angewendet werden: `reasoning_effort`, `active_model`, `active_provider`
    /// und der [`InteractionMode`]. Die `Option<String>`-Speicherung im Controller
    /// wird bei der Anwendung in [`ModelId`] / [`ProviderId`] konvertiert.
    ///
    /// Der Modus wird nur gesetzt, wenn überhaupt einer angefordert wurde — sonst
    /// würde jeder `/model`-Wechsel die Session ungefragt auf den Default-Modus
    /// zurückstellen. Zusätzlich wird [`AgentSession::set_mode`] nur aufgerufen,
    /// wenn der angeforderte Modus vom aktuellen (`session.mode()`) abweicht:
    /// [`AgentSession::set_mode`] schneidet Tool-Aktivierung und Sandbox-Obergrenze
    /// neu aus der Basis (W2A-01), und dieser Schnitt darf nicht bei jedem
    /// `/effort`- oder `/model`-Wechsel wiederholt werden, wenn sich der Modus
    /// dabei gar nicht ändert — sonst würden zur Laufzeit über `activation_mut()`
    /// vorgenommene Overrides bei jedem Controller-Apply verworfen, obwohl kein
    /// Moduswechsel angefordert wurde. Der Aufruf gehört weiterhin zwingend an
    /// die Turn-Grenze und nicht in einen laufenden Turn.
    ///
    /// # Argumente
    /// - `session` (`&mut AgentSession`): die lebende Session, auf die mutiert wird.
    ///
    /// # Rückgabe
    /// `true` wenn die Session modifiziert wurde (Effort, Modell, Provider wurden
    /// stets geschrieben, sobald eine Mutation anstand; der Modus zusätzlich nur
    /// bei tatsächlichem Wechsel), `false` wenn keine Mutation anstand.
    ///
    /// # Nebenläufigkeit
    /// Darf nur zwischen Turns aufgerufen werden (vom Renderer-Thread), nicht
    /// während ein Turn läuft.
    ///
    /// # Spec
    /// harw-tui Design §session_controller — apply_to_session; W2A-01
    /// (`AgentSession::set_mode` schneidet ab jetzt immer von der Basis).
    pub fn apply_to_session(&self, session: &mut AgentSession) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner.generation == inner.applied_generation {
            return false;
        }
        session.set_reasoning_effort(inner.reasoning_effort);
        session.set_active_model(inner.active_model.as_deref().map(ModelId::from));
        session.set_active_provider(inner.active_provider.as_deref().map(ProviderId::from));
        if let Some(mode) = inner.interaction_mode {
            if session.mode() != mode {
                session.set_mode(mode);
            }
        }
        inner.applied_generation = inner.generation;
        true
    }
}

impl Default for TuiSessionController {
    /// Erstellt einen leeren `TuiSessionController` (delegiert an [`Self::new`]).
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TuiSessionController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner.lock() {
            Ok(inner) => formatter
                .debug_struct("TuiSessionController")
                .field("reasoning_effort", &inner.reasoning_effort)
                .field("active_model", &inner.active_model)
                .field("active_provider", &inner.active_provider)
                .field("generation", &inner.generation)
                .field("applied_generation", &inner.applied_generation)
                .finish(),
            Err(_) => formatter
                .debug_struct("TuiSessionController")
                .field("inner", &"<poisoned>")
                .finish(),
        }
    }
}

impl SessionController for TuiSessionController {
    /// Setzt den gewünschten Reasoning-Effort-Level.
    ///
    /// # Argumente
    /// - `effort` (`Option<ReasoningEffort>`): Neuer Level; `None` = Provider-Default.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Interner Mutex vergiftet.
    ///
    /// # Spec
    /// harw-tui Design §session_controller — set_reasoning_effort.
    fn set_reasoning_effort(
        &self,
        effort: Option<ReasoningEffort>,
    ) -> Result<(), SessionControlError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| SessionControlError::Disconnected)?;
        inner.reasoning_effort = effort;
        inner.generation = inner.generation.saturating_add(1);
        Ok(())
    }

    /// Wechselt die aktiv gespeicherte Modell-ID.
    ///
    /// # Argumente
    /// - `model_id` (`String`): Die gewünschte Modell-ID.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Interner Mutex vergiftet.
    ///
    /// # Spec
    /// harw-tui Design §session_controller — set_active_model.
    fn set_active_model(&self, model_id: String) -> Result<(), SessionControlError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| SessionControlError::Disconnected)?;
        inner.active_model = Some(model_id);
        inner.generation = inner.generation.saturating_add(1);
        Ok(())
    }

    /// Wechselt die aktiv gespeicherte Provider-ID.
    ///
    /// # Argumente
    /// - `provider_id` (`String`): Die gewünschte Provider-ID.
    ///
    /// # Fehler
    /// - [`SessionControlError::Disconnected`]: Interner Mutex vergiftet.
    ///
    /// # Spec
    /// harw-tui Design §session_controller — set_active_provider.
    fn set_active_provider(&self, provider_id: String) -> Result<(), SessionControlError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| SessionControlError::Disconnected)?;
        inner.active_provider = Some(provider_id);
        inner.generation = inner.generation.saturating_add(1);
        Ok(())
    }

    /// Merkt den angeforderten Interaktionsmodus für die nächste Turn-Grenze vor.
    ///
    /// # Beschreibung
    /// Überschreibt den fail-closed Trait-Default: die TUI **führt** einen Modus.
    /// Der Name wird hier — und nur hier — mit [`InteractionMode::parse`] geprüft;
    /// ein unbekannter Name wird abgewiesen, statt still auf den Default zu
    /// fallen. Angewandt wird der Modus von [`Self::apply_to_session`] am
    /// Turn-Boundary, weil ein laufender Turn seine Tool-Menge und
    /// Sandbox-Obergrenze nicht unter sich wechseln darf.
    ///
    /// # Argumente
    /// - `mode` (`&str`): Kanonischer Modusname (`chat`, `plan`, `explore`, `work`).
    ///
    /// # Fehler
    /// - [`SessionControlError::ModeUnsupported`]: Name ist kein bekannter Modus.
    /// - [`SessionControlError::Disconnected`]: Interner Mutex vergiftet.
    fn request_mode(&self, mode: &str) -> Result<(), SessionControlError> {
        let parsed = InteractionMode::parse(mode)
            .ok_or_else(|| SessionControlError::ModeUnsupported(mode.to_owned()))?;
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| SessionControlError::Disconnected)?;
        inner.interaction_mode = Some(parsed);
        inner.generation = inner.generation.saturating_add(1);
        Ok(())
    }

    /// Gibt eine Momentaufnahme des aktuellen Controller-Zustands zurück.
    ///
    /// # Rückgabe
    /// [`SessionControlSnapshot`] — geklonter Zustand; bei vergiftetem Mutex
    /// ein leerer Snapshot.
    ///
    /// # Spec
    /// harw-tui Design §session_controller — snapshot.
    fn snapshot(&self) -> SessionControlSnapshot {
        match self.inner.lock() {
            Ok(inner) => SessionControlSnapshot {
                reasoning_effort: inner.reasoning_effort,
                active_model: inner.active_model.clone(),
                active_provider: inner.active_provider.clone(),
                interaction_mode: inner
                    .interaction_mode
                    .map(|mode| mode.as_str().to_owned()),
            },
            Err(_) => SessionControlSnapshot::empty(),
        }
    }
}

// ── SharedTuiSessionController ────────────────────────────────────────────────

/// Bequemer Alias für die ServiceMap-Registration eines [`TuiSessionController`].
///
/// # Beschreibung
/// Ermöglicht nahtloses `.insert(controller.clone())` in eine [`harw_operations::ServiceMap`]
/// ohne expliziten Upcast — der Upcast auf `Arc<dyn SessionController>` geschieht
/// in [`build_services`][crate::command_exec] via `as SharedSessionController`.
///
/// # Spec
/// harw-tui Design §session_controller — SharedTuiSessionController.
pub type SharedTuiSessionController = Arc<TuiSessionController>;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use harw_extension_api::ExtensionRegistryBuilder;
    use harw_types::AgentRole;
    use tokio::sync::mpsc;

    /// Baut eine minimale [`AgentSession`]-Fixture für Tests.
    fn test_session() -> AgentSession {
        let (event_tx, _rx) = mpsc::unbounded_channel();
        AgentSession::new(
            AgentRole::Assistant,
            None,
            ExtensionRegistryBuilder::default().build(),
            event_tx,
        )
    }

    // ── 1. Neuer Controller hat leeren Snapshot ───────────────────────────────

    #[test]
    fn test_new_controller_snapshot_is_empty() {
        let ctrl = TuiSessionController::new();
        let snap = ctrl.snapshot();
        assert!(
            snap.reasoning_effort.is_none(),
            "reasoning_effort must be None on a fresh controller"
        );
        assert!(
            snap.active_model.is_none(),
            "active_model must be None on a fresh controller"
        );
        assert!(
            snap.active_provider.is_none(),
            "active_provider must be None on a fresh controller"
        );
    }

    // ── 2. set_reasoning_effort aktualisiert den Snapshot ────────────────────

    #[test]
    fn test_set_reasoning_effort_updates_snapshot() {
        let ctrl = TuiSessionController::new();
        ctrl.set_reasoning_effort(Some(ReasoningEffort::High))
            .expect("set_reasoning_effort should succeed");
        let snap = ctrl.snapshot();
        assert_eq!(
            snap.reasoning_effort,
            Some(ReasoningEffort::High),
            "snapshot must reflect the set reasoning effort"
        );
    }

    // ── 3. set_active_model aktualisiert den Snapshot ────────────────────────

    #[test]
    fn test_set_active_model_updates_snapshot() {
        let ctrl = TuiSessionController::new();
        ctrl.set_active_model("claude-opus-4".to_owned())
            .expect("set_active_model should succeed");
        let snap = ctrl.snapshot();
        assert_eq!(
            snap.active_model.as_deref(),
            Some("claude-opus-4"),
            "snapshot must reflect the set model id"
        );
    }

    // ── 4. set_active_provider aktualisiert den Snapshot ─────────────────────

    #[test]
    fn test_set_active_provider_updates_snapshot() {
        let ctrl = TuiSessionController::new();
        ctrl.set_active_provider("anthropic".to_owned())
            .expect("set_active_provider should succeed");
        let snap = ctrl.snapshot();
        assert_eq!(
            snap.active_provider.as_deref(),
            Some("anthropic"),
            "snapshot must reflect the set provider id"
        );
    }

    // ── 5. apply_to_session überträgt reasoning_effort auf AgentSession ──────

    #[test]
    fn test_apply_to_session_moves_reasoning_effort_to_agent_session() {
        let ctrl = TuiSessionController::new();
        ctrl.set_reasoning_effort(Some(ReasoningEffort::Medium))
            .expect("set_reasoning_effort should succeed");

        let mut session = test_session();
        assert_eq!(
            session.reasoning_effort(),
            None,
            "session must start with no reasoning effort"
        );

        let modified = ctrl.apply_to_session(&mut session);
        assert!(
            modified,
            "apply_to_session must return true when mutations are pending"
        );
        assert_eq!(
            session.reasoning_effort(),
            Some(ReasoningEffort::Medium),
            "session must reflect the applied reasoning effort"
        );
    }

    // ── 6. apply_to_session ist No-op wenn keine Mutation ansteht ────────────

    #[test]
    fn test_apply_to_session_no_op_when_no_mutation() {
        let ctrl = TuiSessionController::new();
        let mut session = test_session();

        // Erster Apply — ohne jede Mutation.
        let modified = ctrl.apply_to_session(&mut session);
        assert!(
            !modified,
            "apply_to_session must return false when no mutations are pending"
        );
        assert_eq!(
            session.reasoning_effort(),
            None,
            "session must not be mutated when no mutations are pending"
        );
    }

    // ── 7. Zweiter Apply nach Apply ist No-op (generation == applied_generation) ──

    #[test]
    fn test_apply_to_session_second_apply_is_noop_after_drain() {
        let ctrl = TuiSessionController::new();
        ctrl.set_reasoning_effort(Some(ReasoningEffort::Low))
            .expect("set should succeed");

        let mut session = test_session();
        let first = ctrl.apply_to_session(&mut session);
        assert!(first, "first apply must be effective");

        // Session auf High setzen — um zu prüfen, dass der zweite Apply sie nicht
        // wieder auf Low zurücksetzt.
        session.set_reasoning_effort(Some(ReasoningEffort::High));

        let second = ctrl.apply_to_session(&mut session);
        assert!(
            !second,
            "second apply without new mutations must be a no-op"
        );
        assert_eq!(
            session.reasoning_effort(),
            Some(ReasoningEffort::High),
            "session must not be overwritten by a no-op apply"
        );
    }

    // ── 8. with_snapshot vorbelegt Zustand korrekt ───────────────────────────

    #[test]
    fn test_with_snapshot_preseeds_state() {
        let snap = SessionControlSnapshot {
            reasoning_effort: Some(ReasoningEffort::Minimal),
            active_model: Some("test-model".to_owned()),
            active_provider: Some("test-provider".to_owned()),
            interaction_mode: Some("explore".to_owned()),
        };
        let ctrl = TuiSessionController::with_snapshot(snap.clone());
        assert_eq!(
            ctrl.snapshot(),
            snap,
            "with_snapshot must produce a controller whose snapshot matches the input"
        );
    }

    // ── 9. Arc-Clone teilt denselben Zustand ─────────────────────────────────

    #[test]
    fn test_arc_clone_shares_state() {
        let ctrl = Arc::new(TuiSessionController::new());
        let ctrl2 = Arc::clone(&ctrl);

        ctrl.set_active_model("shared-model".to_owned())
            .expect("set_active_model should succeed");

        let snap = ctrl2.snapshot();
        assert_eq!(
            snap.active_model.as_deref(),
            Some("shared-model"),
            "Arc clone must observe mutations made via the original handle"
        );
    }

    // ── 10. set_reasoning_effort None löscht gesetzten Wert ──────────────────

    #[test]
    fn test_set_reasoning_effort_none_clears_previous_value() {
        let ctrl = TuiSessionController::new();
        ctrl.set_reasoning_effort(Some(ReasoningEffort::High))
            .expect("initial set should succeed");
        ctrl.set_reasoning_effort(None)
            .expect("clearing effort should succeed");
        assert_eq!(
            ctrl.snapshot().reasoning_effort,
            None,
            "snapshot must be None after clearing the reasoning effort"
        );
    }

    // ── 11. apply_to_session propagates active_model to AgentSession ──────────

    #[test]
    fn apply_to_session_propagates_active_model() {
        let ctrl = TuiSessionController::new();
        ctrl.set_active_model("claude-opus-4".to_owned())
            .expect("set_active_model should succeed");

        let mut session = test_session();
        assert!(
            session.active_model().is_none(),
            "session must start with no active_model"
        );

        let modified = ctrl.apply_to_session(&mut session);
        assert!(
            modified,
            "apply_to_session must return true when mutations are pending"
        );
        assert_eq!(
            session.active_model(),
            Some(&ModelId::from("claude-opus-4")),
            "session.active_model must equal the model set on the controller"
        );
    }

    // ── 12. apply_to_session propagates active_provider to AgentSession ───────

    #[test]
    fn apply_to_session_propagates_active_provider() {
        let ctrl = TuiSessionController::new();
        ctrl.set_active_provider("anthropic".to_owned())
            .expect("set_active_provider should succeed");

        let mut session = test_session();
        assert!(
            session.active_provider().is_none(),
            "session must start with no active_provider"
        );

        let modified = ctrl.apply_to_session(&mut session);
        assert!(
            modified,
            "apply_to_session must return true when mutations are pending"
        );
        assert_eq!(
            session.active_provider(),
            Some(&ProviderId::from("anthropic")),
            "session.active_provider must equal the provider set on the controller"
        );
    }

    // ── 13. apply_to_session clears active_model when set to None ────────────

    #[test]
    fn apply_to_session_clears_active_model_when_set_to_none() {
        let ctrl = TuiSessionController::new();

        // Prime the session with a model via the controller.
        ctrl.set_active_model("claude-opus-4".to_owned())
            .expect("initial set_active_model should succeed");
        let mut session = test_session();
        ctrl.apply_to_session(&mut session);
        assert_eq!(
            session.active_model(),
            Some(&ModelId::from("claude-opus-4")),
            "precondition: session must have the initial model after first apply"
        );

        // Simulate clearing the model, then a second set to push the generation forward.
        // The controller has no "set_active_model_none" API, so we set a different model
        // and then mutate the inner state via with_snapshot round-trip.
        // Instead: set any value then manually call apply on a fresh controller seeded
        // with active_model = None via with_snapshot.
        let snap_none = SessionControlSnapshot {
            reasoning_effort: None,
            active_model: None,
            active_provider: None,
            interaction_mode: None,
        };
        let ctrl2 = TuiSessionController::with_snapshot(snap_none);
        // with_snapshot initialises both counters to 0, so apply_to_session sees no
        // pending mutation.  We need to trigger a generation bump via any setter.
        ctrl2
            .set_reasoning_effort(None)
            .expect("set_reasoning_effort(None) should succeed to bump generation");

        let modified = ctrl2.apply_to_session(&mut session);
        assert!(modified, "apply must return true after setter call");
        assert!(
            session.active_model().is_none(),
            "session.active_model must be None after applying a None snapshot"
        );
    }

    // ── 14. apply_to_session mit unverändertem Modus lässt die Aktivierung
    //        unangetastet (W2A-01: set_mode schneidet immer von der Basis) ────

    #[test]
    fn test_apply_to_session_same_mode_does_not_reset_activation() {
        use harw_tools::ToolName;

        let mut session = test_session();
        assert_eq!(
            session.mode(),
            InteractionMode::Chat,
            "precondition: fresh session starts in Chat mode"
        );

        // Aktivierung manuell einschränken — simuliert einen zur Laufzeit über
        // `activation_mut()` gesetzten Override, der bei einem unnötigen
        // `set_mode`-Aufruf verloren ginge.
        session
            .activation_mut()
            .disable_tool(ToolName::new("shell.exec"));
        assert!(
            !session.activation().is_tool_enabled(&ToolName::new("shell.exec")),
            "precondition: tool override must be in effect before apply"
        );

        let ctrl = TuiSessionController::new();
        ctrl.request_mode("chat")
            .expect("chat must be a known mode");

        let modified = ctrl.apply_to_session(&mut session);
        assert!(
            modified,
            "apply_to_session must still report true: a mutation was pending"
        );
        assert_eq!(
            session.mode(),
            InteractionMode::Chat,
            "mode must remain Chat (no change requested)"
        );
        assert!(
            !session.activation().is_tool_enabled(&ToolName::new("shell.exec")),
            "requesting the session's current mode must not re-cut activation \
             from the base and must not undo the manual restriction"
        );
    }

    // ── 15. apply_to_session mit geändertem Modus wendet den Modus an ────────

    #[test]
    fn test_apply_to_session_changed_mode_applies_mode() {
        let mut session = test_session();
        assert_eq!(
            session.mode(),
            InteractionMode::Chat,
            "precondition: fresh session starts in Chat mode"
        );

        let ctrl = TuiSessionController::new();
        ctrl.request_mode("explore")
            .expect("explore must be a known mode");

        let modified = ctrl.apply_to_session(&mut session);
        assert!(
            modified,
            "apply_to_session must return true when a mode change is pending"
        );
        assert_eq!(
            session.mode(),
            InteractionMode::Explore,
            "an actual mode change must still be applied via set_mode"
        );
    }
}
