//! ChatApp methods: history access.

use super::*;

impl ChatApp {
    /// Hängt eine typisierte Zelle für die gegebene Rolle und den Text an den Log an.
    ///
    /// # Beschreibung
    /// Wählt anhand der `role` die passende interne `HistoryCell`-Implementierung:
    /// - [`Role::User`] → `UserHistoryCell`
    /// - [`Role::Assistant`] → `AssistantHistoryCell`
    /// - [`Role::System`] → `PlainHistoryCell`
    ///
    /// Die erzeugte Zelle wird am Ende von `cells` angehängt.
    /// Der Scroll-Zustand wird nicht verändert: am Ende folgt die Ansicht,
    /// hochgescrollt bleibt der sichtbare Text stehen (Anker-Nachführung beim
    /// Zeichnen, siehe [`ChatScroll::sync_layout`]).
    ///
    /// # Argumente
    /// - `role` ([`Role`]): Rolle der Nachricht; bestimmt das Rendering-Format.
    /// - `text` (`impl Into<String>`): Nachrichtentext; Ownership wird übernommen.
    pub fn push_line(&mut self, role: Role, text: impl Into<String>) {
        let text = text.into();
        // Eine neue User-/Assistant-/System-Zeile schließt jede offene
        // Lese-Gruppe (Plan Schritt 2 „Gruppierung": „sobald … Assistententext
        // kommt, wird die Gruppe geschlossen").
        self.close_tool_group();
        // PL-97 M3: eine Nutzernachricht beginnt einen neuen Turn (auch beim
        // Resume, wo keine Turn-Kennungen vorliegen).
        if role == Role::User {
            self.activity.next_turn();
        }
        match role {
            // Eingespeiste Hintergrund-Meldungen stehen im Verlauf als
            // Nutzernachricht, im Export aber nicht unter `## Du`.
            Role::User => self
                .export_entries
                .extend(background_agents::user_text_export_entries(&text)),
            Role::Assistant => self
                .export_entries
                .push(ExportEntry::Assistant(text.clone())),
            Role::System => self.export_entries.push(ExportEntry::System(text.clone())),
        }
        // PL-97 C5: Lage der Nachricht für die Slot-Tabelle der Live-Zellen.
        self.note_live_slot_message(role);
        let cell: Box<dyn HistoryCell> = match role {
            Role::User => Box::new(UserHistoryCell { text }),
            Role::Assistant => Box::new(AssistantHistoryCell { source: text }),
            Role::System => Box::new(PlainHistoryCell {
                lines: vec![Line::from(text)],
            }),
        };
        self.cells.push(cell);
        // Neuer Inhalt: kein Scroll-Sprung (Anker folgt in `sync_layout`).
        self.scroll.on_new_content();
    }

    /// Hängt eine `PlainHistoryCell` mit vorgerenderten Zeilen an den Log an.
    ///
    /// # Beschreibung
    /// Wird intern verwendet, um mehrzeilige System-/Assistenten-Ausgaben direkt
    /// als bereits berechnete [`Line`]-Vektoren anzuhängen. Kein Scroll-Sprung
    /// (siehe [`ChatScroll::sync_layout`]).
    ///
    /// # Argumente
    /// - `lines` (`Vec<Line<'static>>`): Vorgerenderte Zeilen der Zelle.
    pub(crate) fn push_lines(&mut self, lines: Vec<Line<'static>>) {
        self.cells.push(Box::new(PlainHistoryCell { lines }));
        self.scroll.on_new_content();
    }

    /// Hängt eine bereits gebaute, unveränderliche Zelle an den Verlauf an.
    ///
    /// # Argumente
    /// - `cell` (`Box<dyn HistoryCell>`): die anzuhängende Zelle.
    pub(crate) fn push_cell(&mut self, cell: Box<dyn HistoryCell>) {
        self.cells.push(cell);
        self.scroll.on_new_content();
    }

    /// Gibt die Anzahl der gespeicherten Verlauf-Zellen zurück.
    ///
    /// # Rückgabe
    /// Anzahl der `HistoryCell`-Einträge im internen Log (`cells.len()`).
    #[must_use]
    pub fn cells_len(&self) -> usize {
        self.cells.len()
    }

    /// Gibt den aktuellen Eingabepuffer zurück.
    ///
    /// # Rückgabe
    /// Der aktuell getippte, noch nicht abgeschickte Text.
    #[must_use]
    pub fn input(&self) -> &str {
        self.input.text()
    }

    /// Gibt die über die Session-Laufzeit aufsummierte Token-Nutzung zurück.
    ///
    /// # Beschreibung
    /// Wird bei jedem `SessionEvent::TurnCompleted` in `run_loop` via
    /// [`TokenUsage::add`] aktualisiert und in der Statuszeile ([`status_line`])
    /// als kompakter Suffix angezeigt.
    ///
    /// # Rückgabe
    /// Referenz auf die intern akkumulierte [`TokenUsage`].
    #[must_use]
    pub fn total_usage(&self) -> &TokenUsage {
        &self.total_usage
    }
}
