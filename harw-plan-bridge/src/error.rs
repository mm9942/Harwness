//! Fehlertypen für `harw-plan-bridge`.
//!
//! # Verantwortungsbereich
//! Definiert das crate-weite [`PlanBridgeError`]-Enum. Es ist bewusst die
//! *Vereinigung* der Fehlerquellen, die an der Naht zwischen Goal, Plan,
//! Findings und Jobs zusammenlaufen: Plan-Mutationen (`harw-plan`),
//! Recherche-Parsing (`harw-research`), Dateisystem (Finding-Artefakte),
//! JSON (Job-Payloads) und die Bridge-eigenen Vertragsverletzungen.
//!
//! `Display`, `Debug` (über `Display`), `std::error::Error` inklusive
//! `source()` sowie die `From`-Konvertierungen entstehen aus
//! `#[derive(harw_macros::HarwError)]` — Muster: `harw-plan/src/error.rs`.
//! Weil der Enum-Name auf `Error` endet, erzeugt das Makro zusätzlich den
//! Typalias [`PlanBridgeResult`]; eine handgeschriebene `pub type`-Zeile
//! entfällt.
//!
//! Kein `anyhow`, kein `thiserror`.
//!
//! # Exportierte Typen
//! [`PlanBridgeError`], [`PlanBridgeResult`].
//!
//! # Concurrency
//! Reiner Werttyp; `Send + Sync`, solange die gewrappten Fremdfehler es sind
//! (`std::io::Error`, `serde_json::Error`, `PlanError`, `ResearchError` sind
//! es alle).
//!
//! # Spezifikation
//! AP W3-06..13; philosophy.md §5/§15; coding-philosophy.md §4 (Fehler tragen
//! ihren Kontext), §6–§8.

use harw_macros::HarwError;

use harw_plan::TaskId;

/// Alle Fehler, die beim Verbinden von Goal, Plan, Findings und Jobs entstehen.
///
/// # Description
/// Jede Variante trägt so viel Kontext, dass die Ursache ohne Blick in den
/// Quelltext verständlich ist. Varianten, die einen Fremdfehler wrappen,
/// tragen `#[from]`, damit `?` ohne `map_err` funktioniert und `source()` auf
/// die eigentliche Ursache zeigt.
///
/// # Concurrency
/// `Send + Sync`; enthält keine Locks und keine inneren Referenzen.
#[derive(Debug, HarwError)]
pub enum PlanBridgeError {
    /// Eine Plan- oder Goal-Mutation wurde von `harw-plan` abgelehnt.
    #[msg("Plan-Fehler: {0}")]
    #[from]
    Plan(harw_plan::error::PlanError),

    /// Ein Recherche-Ergebnis ließ sich nicht parsen oder verletzt den
    /// Recherche-Vertrag.
    #[msg("Recherche-Fehler: {0}")]
    #[from]
    Research(harw_research::ResearchError),

    /// Dateisystemfehler beim Lesen oder Schreiben eines Finding-Artefakts.
    #[msg("I/O-Fehler: {0}")]
    #[from]
    Io(std::io::Error),

    /// Ein Job-Payload oder ein Finding-Dokument war kein gültiges JSON.
    #[msg("Serialisierungsfehler: {0}")]
    #[from]
    Json(serde_json::Error),

    /// Der Job-Store hat die Aufnahme oder Aktualisierung abgelehnt.
    ///
    /// Trägt die `Display`-Form des ursprünglichen `SessionStoreError` bzw.
    /// `JobRuntimeError` als Text: beide Crates liegen unterhalb dieser
    /// Bridge, aber ihre Fehlertypen sind hier nicht Teil des öffentlichen
    /// Vertrags.
    #[msg("Job-Store-Fehler: {0}")]
    JobStore(String),

    /// Ein für die Operation nötiger Dienst ist im `OpContext` nicht
    /// registriert — die Composition-Root hat
    /// `register_plan_services` nicht (vollständig) aufgerufen.
    #[msg("Dienst '{service}' ist im OpContext nicht registriert")]
    ServiceMissing {
        /// Name des fehlenden Dienstes (`plan_store`, `goal_store`, …).
        service: &'static str,
    },

    /// Der adressierte Plan-Knoten existiert im aktuellen Plan nicht.
    #[msg("Plan-Knoten '{task}' existiert im aktuellen Plan nicht")]
    NodeNotFound {
        /// Der nicht auffindbare Knoten.
        task: TaskId,
    },

    /// Eine Goal-Aussage wurde verlangt, obwohl kein Goal an den Plan
    /// gebunden ist.
    #[msg("Kein Goal an diesen Plan gebunden")]
    GoalUnbound,

    /// Das `members_from_plan`-Muster einer Zelle ist unbrauchbar.
    #[msg("Zell-Mitgliedermuster '{pattern}' ist unbrauchbar (leer oder nur Leerzeichen)")]
    CellMemberPattern {
        /// Das abgelehnte Muster, wie es in der Agent-DSL stand.
        pattern: String,
    },

    /// Es gibt aktuell keinen ausführbaren Knoten, der als Job admittiert
    /// werden könnte.
    ///
    /// Bewusst ein Fehler und kein leeres `Vec`: eine leere Welle zu starten
    /// ist fast immer ein Denkfehler des Aufrufers und soll nicht still
    /// durchrutschen.
    #[msg("Kein ausführbarer Plan-Knoten vorhanden, der als Job admittiert werden könnte")]
    NoReadyNodes,

    /// Ein Sicherheitsbefund qualifiziert sich nicht für die Plan-Andockung
    /// (Knoten AW6-04, [`crate::security_bridge`]): [`harw_dod_escalate::Ladder::stage_for_or_reject`]
    /// hat abgelehnt. Die gewrappte [`harw_dod_escalate::EscalateError`] trägt
    /// selbst nur eine feste, nicht interpolierte Meldung — diese Variante
    /// fügt keinen zusätzlichen Inhalt hinzu, sie bettet nur ein.
    #[msg("Sicherheitsbefund qualifiziert sich nicht für eine Eskalationsstufe: {0}")]
    #[from]
    SecurityDocking(harw_dod_escalate::EscalateError),
}

/// Übersetzt einen `harw-knowledge`-Fehler in einen Bridge-Fehler.
///
/// # Description
/// `write_atomic` ist der einzige Berührungspunkt mit `harw-knowledge`. Dessen
/// `KnowledgeError` ist im Kern ein I/O-Fehler; nur die Frontmatter-Varianten
/// sind es nicht, und für die bleibt der Text als `io::Error::other` erhalten,
/// statt eine eigene Bridge-Variante für einen Fremdfehler zu erfinden.
///
/// # Arguments
/// - `error` (`harw_knowledge::KnowledgeError`): der zu übersetzende Fehler.
///
/// # Returns
/// [`PlanBridgeError::Io`] mit erhaltener Ursache bzw. erhaltenem Text.
pub(crate) fn map_knowledge_error(error: harw_knowledge::KnowledgeError) -> PlanBridgeError {
    match error {
        harw_knowledge::KnowledgeError::Io(io) => PlanBridgeError::Io(io),
        other => PlanBridgeError::Io(std::io::Error::other(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_missing_display_names_service() {
        let error = PlanBridgeError::ServiceMissing {
            service: "plan_store",
        };
        assert_eq!(
            error.to_string(),
            "Dienst 'plan_store' ist im OpContext nicht registriert"
        );
    }

    #[test]
    fn test_node_not_found_display_names_task() {
        let error = PlanBridgeError::NodeNotFound {
            task: TaskId::new("t-42"),
        };
        assert_eq!(
            error.to_string(),
            "Plan-Knoten 't-42' existiert im aktuellen Plan nicht"
        );
    }

    #[test]
    fn test_io_error_is_converted_via_from_and_keeps_source() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "kein Zugriff");
        let error = PlanBridgeError::from(io);
        assert!(matches!(error, PlanBridgeError::Io(_)));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn test_plan_error_is_converted_via_from() {
        let plan_error = harw_plan::error::PlanError::GoalNotFound;
        let error = PlanBridgeError::from(plan_error);
        assert!(matches!(error, PlanBridgeError::Plan(_)));
    }

    #[test]
    fn test_cell_member_pattern_display_names_pattern() {
        let error = PlanBridgeError::CellMemberPattern {
            pattern: "   ".to_owned(),
        };
        assert!(error.to_string().contains("'   '"));
    }

    #[test]
    fn test_no_ready_nodes_has_a_message() {
        assert!(
            PlanBridgeError::NoReadyNodes
                .to_string()
                .contains("Kein ausführbarer Plan-Knoten")
        );
    }

    #[test]
    fn test_security_docking_error_wraps_content_free_message() {
        let error = PlanBridgeError::from(harw_dod_escalate::EscalateError::NotEscalatable);
        let text = error.to_string();
        assert!(text.contains("Sicherheitsbefund qualifiziert sich nicht"));
        assert!(!text.chars().any(|c| c.is_ascii_digit()));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn test_map_knowledge_error_preserves_io_cause() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "weg");
        let mapped = map_knowledge_error(harw_knowledge::KnowledgeError::Io(io));
        match mapped {
            PlanBridgeError::Io(inner) => assert_eq!(inner.kind(), std::io::ErrorKind::NotFound),
            other => panic!("unerwartete Variante: {other}"),
        }
    }
}
