//! ChatApp methods: permission state.

use super::*;

impl ChatApp {
    /// Leitet die aktuell wirksame [`PermissionCycleStage`] aus dem
    /// Freigabemodus der Montage und dem aktiven Interaktionsmodus ab (Plan
    /// Schritt 5).
    ///
    /// # Rückgabe
    /// [`PermissionCycleStage::Plan`], wenn die Sitzung im Plan-Modus ist;
    /// sonst die Stufe, die dem aktuellen [`ApprovalMode`] entspricht. Ohne
    /// Runtime-Montage gilt [`PermissionCycleStage::Ask`] als sicherer Default.
    #[must_use]
    pub(crate) fn current_permission_stage(&self) -> PermissionCycleStage {
        if self.active_mode == InteractionMode::Plan {
            return PermissionCycleStage::Plan;
        }
        match self.runtime.as_ref().map(|rt| rt.approval_mode().get()) {
            Some(ApprovalMode::AlwaysAsk) | None => PermissionCycleStage::Ask,
            Some(ApprovalMode::Delegated) => PermissionCycleStage::Auto,
            Some(ApprovalMode::FullAccess) => PermissionCycleStage::Full,
        }
    }

    /// Gibt die vorgemerkte, noch nicht angewendete Zyklus-Stufe zurück, falls
    /// ein Wechsel während eines laufenden Turns angefordert wurde (Plan
    /// Schritt 5).
    #[must_use]
    pub(crate) fn pending_permission_stage(&self) -> Option<PermissionCycleStage> {
        self.pending_permission_stage
    }

    /// Schreibt einen neuen [`ApprovalMode`] in die geteilte Zelle der
    /// Montage; ohne Montage ein No-op (es gibt dann nichts zu schreiben).
    pub(super) fn set_approval_mode(&self, mode: ApprovalMode) {
        if let Some(rt) = self.runtime.as_ref() {
            rt.approval_mode().set(mode);
        }
    }

    /// Setzt eine Stufe des Shift+Tab-Zyklus sofort um (Plan Schritt 5).
    ///
    /// # Beschreibung
    /// `Plan` merkt sich den bisherigen Interaktionsmodus
    /// (`Self::mode_before_plan`) und fordert [`InteractionMode::Plan`] an;
    /// jede andere Stufe setzt nur den Freigabemodus — kehrt der Zyklus dabei
    /// gerade aus `Plan` zurück, wird zusätzlich der gemerkte vorherige Modus
    /// angefordert. Beide Anforderungen laufen über
    /// [`harw_operations::SessionController::request_mode`] und wirken daher
    /// erst an der nächsten Turn-Grenze (AP W5-05, siehe
    /// [`Self::apply_pending_controller_state`]).
    ///
    /// # Argumente
    /// - `stage` ([`PermissionCycleStage`]): die anzuwendende Zielstufe.
    pub(super) fn apply_permission_stage(&mut self, stage: PermissionCycleStage) {
        match stage {
            PermissionCycleStage::Plan => {
                if self.mode_before_plan.is_none() {
                    self.mode_before_plan = Some(self.active_mode);
                }
                // Runde 5: `Delegated` statt `AlwaysAsk` — Lesen und
                // `plan.write` (nur `.harw/plans`) laufen ohne Rückfrage, alles
                // andere sperrt die Plan-Sperre (`PlanModeGate`).
                self.set_approval_mode(ApprovalMode::Delegated);
                self.active_mode = InteractionMode::Plan;
                if let Err(error) = self
                    .session_controller
                    .request_mode(InteractionMode::Plan.as_str())
                {
                    tracing::warn!(
                        error = %error,
                        "tui.permission_cycle.request_plan_mode_failed"
                    );
                }
            }
            PermissionCycleStage::Ask => self.finish_permission_stage(ApprovalMode::AlwaysAsk),
            PermissionCycleStage::Auto => self.finish_permission_stage(ApprovalMode::Delegated),
            PermissionCycleStage::Full => self.finish_permission_stage(ApprovalMode::FullAccess),
        }
        // Runde 5, Teil F: die Plan-Sperre wirkt sofort, auch mitten im Turn.
        self.sync_plan_lock();
    }

    // Gemeinsamer Abschluss der drei Nicht-Plan-Stufen: setzt den Freigabemodus
    // und stellt, falls der Zyklus gerade aus `Plan` zurückkehrt, den vorherigen
    // Interaktionsmodus wieder her. Vormals ein zweites, strukturell
    // überflüssiges `match stage` mit einem `unreachable!`-Arm für `Plan` (der
    // Fall existiert jetzt gar nicht mehr, da jede Nicht-Plan-Stufe ihren
    // eigenen Match-Arm mit fixem `approval`-Wert hat).
    pub(super) fn finish_permission_stage(&mut self, approval: ApprovalMode) {
        self.set_approval_mode(approval);
        // Runde 9, E6: stand die Sitzung schon beim Start im Plan-Modus
        // (`--mode plan`, `/resume`), gibt es keinen gemerkten Vormodus — der
        // Zyklus blieb dann in `plan` hängen, obwohl die Statuszeile eine
        // Freigabestufe zeigte. Verlassen wird `plan` dann nach `work`, wie
        // bei der Planfreigabe.
        let previous = self.mode_before_plan.take().or_else(|| {
            (self.active_mode == InteractionMode::Plan).then_some(InteractionMode::Work)
        });
        if let Some(previous) = previous {
            self.active_mode = previous;
            if let Err(error) = self.session_controller.request_mode(previous.as_str()) {
                tracing::warn!(
                    error = %error,
                    "tui.permission_cycle.restore_mode_failed"
                );
            }
        }
    }

    /// Verarbeitet einen Shift+Tab-Druck: zyklischer Wechsel
    /// `ask → auto → full → plan → ask` (Plan Schritt 5).
    ///
    /// # Argumente
    /// - `turn_running` (`bool`): `true`, wenn gerade ein Turn läuft — der
    ///   Zielzustand wird dann nur vorgemerkt (AP W5-05) und die Statuszeile
    ///   zeigt „(ab nächstem Turn)", statt sofort zu wirken.
    pub(crate) fn cycle_permission_stage(&mut self, turn_running: bool) {
        let current = self
            .pending_permission_stage
            .unwrap_or_else(|| self.current_permission_stage());
        let next = current.next();
        // Freigabestufen gelten sofort, auch während ein Turn läuft. Der
        // Interaktionsmodus wird vom Kern weiter an der Turn-Grenze übernommen,
        // aber der sichtbare Zielzustand aktualisiert sich unverzüglich.
        let _ = turn_running;
        self.pending_permission_stage = None;
        self.apply_permission_stage(next);
    }
}
