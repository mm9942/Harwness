//! ChatApp methods: conversation state.

use super::*;

impl ChatApp {
    /// Setzt die Darstellung der Reasoning-Zusammenfassungen (`[tui]
    /// reasoning`, PL-97 R1; Builder, Composition-Root).
    ///
    /// # Argumente
    /// - `mode` ([`ReasoningDisplayMode`]): `message`, `collapsed` oder `hidden`.
    #[must_use]
    pub(crate) fn with_reasoning_mode(mut self, mode: ReasoningDisplayMode) -> Self {
        self.reasoning_mode = mode;
        self
    }

    /// Die aktive Darstellung der Reasoning-Zusammenfassungen (nur Tests,
    /// etwa die Verdrahtung in `runtime_root`).
    #[cfg(test)]
    pub(crate) fn reasoning_mode(&self) -> ReasoningDisplayMode {
        self.reasoning_mode
    }

    /// Hängt eine Reasoning-Zelle an den Verlauf und merkt sie für Ctrl+O
    /// vor (Runde 2 / Welle 2; PL-97 R1: Startzustand nach `[tui] reasoning`).
    ///
    /// `message` startet ausgeklappt, `collapsed` eingeklappt; bei `hidden`
    /// entsteht keine Zelle (der Export-Eintrag entsteht beim Aufrufer).
    ///
    /// # Argumente
    /// - `summary` (`String`): Zusammengefasster Denkprozess-Text.
    /// - `origin` (`Option<&str>`): Rolle eines Kind-Agenten; `None` für den
    ///   Hauptagenten.
    pub(super) fn push_reasoning_cell(&mut self, summary: String, origin: Option<&str>) {
        let expanded = match self.reasoning_mode {
            ReasoningDisplayMode::Message => true,
            ReasoningDisplayMode::Collapsed => false,
            ReasoningDisplayMode::Hidden => return,
        };
        let mut cell = crate::i18n::with_language(self.language, || {
            ReasoningHistoryCell::new(summary).with_expanded(expanded)
        });
        if let Some(role) = origin {
            cell = cell.with_origin(role);
        }
        let shared = cell.into_shared();
        self.push_cell(Box::new(Arc::clone(&shared)));
        self.tool_cells.push(ToolCellHandle::Reasoning(shared));
    }

    /// Hängt eine Plan-Zelle an (R18 F4): Vollbild beim ersten Stand eines
    /// Plans, sonst das Delta zum zuletzt gezeigten Stand; merkt sie für
    /// Ctrl+O vor.
    pub(super) fn push_plan_cell(&mut self, plan: harw_plan::Plan) {
        let cell =
            crate::i18n::with_language(self.language, || match self.last_shown_plan.as_ref() {
                Some(previous) => PlanGraphCell::delta(previous, plan.clone()),
                None => PlanGraphCell::full(plan.clone()),
            });
        self.last_shown_plan = Some(plan);
        let shared = cell.into_shared();
        self.push_cell(Box::new(Arc::clone(&shared)));
        self.tool_cells.push(ToolCellHandle::Plan(shared));
    }

    /// Gibt `true` zurück, wenn mindestens eine Werkzeug- oder Reasoning-Zelle eingeklappt ist
    /// (Statuszeilen-Hinweis auf Ctrl+O, Plan Schritt 5).
    #[must_use]
    pub(super) fn has_collapsed_tool_cells(&self) -> bool {
        self.tool_cells.iter().any(|handle| !handle.is_expanded())
    }

    /// Gibt den zuletzt beobachteten Interaktionsmodus zurück.
    ///
    /// # Rückgabe
    /// Der [`InteractionMode`], den die Statuszeile anzeigt.
    #[must_use]
    pub(crate) fn active_mode(&self) -> InteractionMode {
        self.active_mode
    }

    /// R10 Welle 3B: der Titel des kompilierten Agenten für die Statuszeile,
    /// falls [`Self::with_title_override`] gesetzt wurde.
    #[must_use]
    pub(crate) fn title_override(&self) -> Option<&str> {
        self.title_override.as_deref()
    }

    /// Übernimmt einen beobachteten Moduswechsel in den Anzeigezustand.
    ///
    /// # Argumente
    /// - `mode` ([`InteractionMode`]): der jetzt gültige Modus.
    pub(crate) fn set_active_mode(&mut self, mode: InteractionMode) {
        self.active_mode = mode;
        // Runde 5, Teil F: `/mode plan` & Co. ziehen die Plan-Sperre nach.
        self.sync_plan_lock();
    }

    /// Hängt eine geteilte, später noch veränderbare Zelle an den Verlauf an.
    ///
    /// # Beschreibung
    /// Der Aufrufer behält einen `Arc`-Klon und kann die Zelle dadurch nach dem
    /// Anhängen weiter fortschreiben — siehe [`SharedHistoryCell`]. Der Verlauf
    /// hält genau **eine** Instanz.
    ///
    /// # Argumente
    /// - `cell` (`Arc<Mutex<T>>`): die geteilte Zelle.
    pub(crate) fn push_shared_cell<T>(&mut self, cell: Arc<Mutex<T>>)
    where
        T: HistoryCell + 'static,
    {
        self.cells.push(Box::new(SharedHistoryCell { inner: cell }));
        self.scroll.on_new_content();
    }
}
