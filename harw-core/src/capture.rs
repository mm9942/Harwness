//! Vertrauenswürdiger Haken, über den das Ergebnis jedes ausgeführten
//! Tool-Aufrufs die Projektgedächtnis-Erfassung erreicht.
//!
//! # Zweck
//! Dieses Modul definiert nur die Schnittstelle — nicht die Implementierung.
//! Die Turn-Loop ruft [`ToolOutcomeObserver::on_tool_outcome`] synchron auf,
//! sobald das endgültige Ergebnis eines Tool-Aufrufs feststeht (erfolgreich
//! ausgeführt, mit Fehler beendet oder abgelehnt/verweigert). Beobachter
//! müssen deshalb billig sein und dürfen den Turn niemals scheitern lassen:
//! jede Implementierung fängt eigene Fehler intern ab (z. B. nur `warn!`
//! loggen) statt sie zu propagieren — die Methode gibt bewusst nichts
//! zurück, das ein Aufrufer auswerten müsste.
//!
//! Die konkrete Erfassung (Projektgedächtnis-Ablage, Datei-Index, …) lebt in
//! `harw-memory` und `harw-runtime`; dieses Modul kennt nur den Vertrag.

use harw_types::SessionId;

/// Ergebnis-Status eines ausgeführten Tool-Aufrufs, wie ihn die Turn-Loop
/// an einen [`ToolOutcomeObserver`] meldet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcomeStatus {
    /// Der Tool-Aufruf wurde ausgeführt und lieferte ein Erfolgsergebnis.
    Success,
    /// Der Tool-Aufruf schlug fehl oder wurde abgelehnt/verweigert.
    Error,
}

/// Das Ergebnis eines einzelnen, ausgeführten Tool-Aufrufs — die Nutzlast,
/// die die Turn-Loop synchron an [`ToolOutcomeObserver::on_tool_outcome`]
/// übergibt.
#[derive(Debug, Clone, Copy)]
pub struct ToolOutcome<'a> {
    /// Name des aufgerufenen Tools.
    pub tool_name: &'a str,
    /// Die vom Modell übergebenen Argumente des Aufrufs.
    pub arguments: &'a serde_json::Value,
    /// Ob der Aufruf erfolgreich war oder fehlschlug.
    pub status: ToolOutcomeStatus,
    /// Textuelle Darstellung des Ergebnisses (Erfolgstext, JSON-Kompaktform
    /// oder Fehlermeldung — siehe Aufrufer in der Turn-Loop).
    pub output_text: &'a str,
}

/// Beobachter, der über das Ergebnis jedes ausgeführten Tool-Aufrufs einer
/// Session benachrichtigt wird.
///
/// # Beschreibung
/// Wird synchron in der Turn-Loop aufgerufen, an der einen Stelle, an der
/// das endgültige Ergebnis eines Tool-Aufrufs feststeht. Implementierungen
/// müssen `Send + Sync` sein (Session wird zwischen Threads geteilt) und
/// dürfen keine Fehler propagieren: eine fehlschlagende Erfassung darf nie
/// den laufenden Turn beeinträchtigen.
pub trait ToolOutcomeObserver: Send + Sync {
    /// Wird für jeden ausgeführten Tool-Aufruf einmal aufgerufen, sobald sein
    /// endgültiges Ergebnis feststeht.
    ///
    /// # Arguments
    /// - `session_id` (`&SessionId`): Identifiziert die Session, in der der
    ///   Aufruf stattfand.
    /// - `outcome` (`&ToolOutcome<'_>`): Name, Argumente, Status und
    ///   Ergebnistext des Aufrufs.
    ///
    /// # Nebenläufigkeit
    /// Muss billig sein und darf nicht blockieren; wird synchron aus dem
    /// Turn-Loop-Pfad heraus aufgerufen.
    fn on_tool_outcome(&self, session_id: &SessionId, outcome: &ToolOutcome<'_>);

    /// Wird am erfolgreichen Ende eines Turns aufgerufen, nachdem alle
    /// Tool-Ergebnisse dieses Turns bereits über [`Self::on_tool_outcome`]
    /// gemeldet wurden. Standardmäßig ein No-op.
    fn on_turn_finished(&self, session_id: &SessionId) {
        let _ = session_id;
    }

    /// Wird am erfolgreichen Ende eines Turns mit dem Text der letzten
    /// Nutzernachricht aufgerufen (vor [`Self::on_turn_finished`]). Grundlage
    /// der Rückmeldung zu gelieferten Gedächtnisfakten (hat der Nutzer
    /// korrigiert?). Standardmäßig ein No-op.
    fn on_user_message(&self, session_id: &SessionId, text: &str) {
        let _ = (session_id, text);
    }

    /// Wird am erfolgreichen Ende eines Turns mit dem Text der letzten
    /// Assistentenantwort aufgerufen (vor [`Self::on_turn_finished`]).
    /// Grundlage der Rückmeldung zu gelieferten Gedächtnisfakten (wurde ein
    /// Fakt genutzt?). Standardmäßig ein No-op.
    fn on_assistant_message(&self, session_id: &SessionId, text: &str) {
        let _ = (session_id, text);
    }
}
