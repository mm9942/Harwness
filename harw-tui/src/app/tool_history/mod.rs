//! ChatApp methods: tool history.

use super::*;

impl ChatApp {
    /// Schließt die aktuell offene Lese-Gruppe (Plan Schritt 2 „Gruppierung").
    ///
    /// # Beschreibung
    /// Aufgerufen, sobald ein anderes Werkzeug, Assistententext oder eine
    /// sonstige Verlaufszelle angehängt wird — die nächste passende `fs.*`-
    /// Anfrage beginnt danach eine neue Gruppe statt der alten beizutreten.
    pub(super) fn close_tool_group(&mut self) {
        self.open_tool_group = None;
    }

    /// Hängt eine neue, geteilte Werkzeugzelle an den Verlauf an und gruppiert
    /// sie nach dem Aktivitäts-Reducer (PL-97 M3, Plan Schritt 2).
    ///
    /// # Beschreibung
    /// Der Reducer ([`activity_glue::append_tool_cell`]) entscheidet über
    /// die Aktivitätsdeskriptoren der Werkzeuge: beobachtende Aufrufe auf
    /// Pfaden treten der offenen Lese-Gruppe bei (oder eröffnen eine),
    /// Bearbeitungen eines Pfads und Jobs bekommen eine eigene, an Ort und
    /// Stelle fortgeschriebene Gruppe, alles andere bleibt eine einzelne
    /// Zelle und schließt die offene Lese-Gruppe. Nur Top-Level-Zellen/
    /// -Gruppen bekommen ein [`ToolCellHandle`] in `self.tool_cells`
    /// (Ctrl+O „alle").
    ///
    /// # Argumente
    /// - `call` (`&ToolCall`): Kennung, Name und Argumente des Aufrufs.
    /// - `turn` (`Option<&str>`): Turn-Kennung eines Live-Aufrufs, `None`
    ///   für den Freigabe- und den Resume-Pfad.
    /// - `cell` ([`SharedToolCell`]): die neu erzeugte, geteilte Zelle.
    pub(super) fn append_tool_cell(&mut self, call: &ToolCall, turn: Option<&str>, cell: SharedToolCell) {
        activity_glue::append_tool_cell(self, call, turn, cell);
    }

    /// Verarbeitet Ctrl+O: klappt die zuletzt angehängte Werkzeugzelle auf/zu,
    /// ein unmittelbar folgender zweiter Druck klappt stattdessen alle um
    /// (Plan Schritt 2).
    ///
    /// # Rückgabe
    /// `true`, wenn mindestens eine Zelle vorhanden war (Redraw nötig);
    /// `false` bei leerem Verlauf.
    pub(crate) fn toggle_tool_cells(&mut self) -> bool {
        if self.tool_cells.is_empty() {
            return false;
        }
        if self.ctrl_o_expand_last_armed {
            let target = !self.tool_cells.iter().all(ToolCellHandle::is_expanded);
            for handle in &self.tool_cells {
                handle.set_expanded(target);
            }
            self.ctrl_o_expand_last_armed = false;
        } else {
            if let Some(last) = self.tool_cells.last() {
                last.toggle_expanded();
            }
            self.ctrl_o_expand_last_armed = true;
        }
        true
    }
}
