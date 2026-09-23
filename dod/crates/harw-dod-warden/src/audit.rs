//! Das Audit-Protokoll: [`AuditEvent`], [`AuditSink`], [`RecordingAuditSink`].
//!
//! # Verantwortungsbereich
//! „Was protokolliert werden soll, wird protokolliert, **bevor** die Aktion
//! versucht wird" (Brief). [`crate::warden::Warden::handle`] ruft
//! [`AuditSink::record`] deshalb an drei Stellen auf, in dieser Reihenfolge:
//!
//! 1. **Vor** jedem Ausführungsversuch: [`AuditEvent::Attempting`] — auch
//!    wenn die anschließende Ausführung fehlschlägt, existiert bereits ein
//!    Eintrag, der die versuchte Aktion nennt.
//! 2. **Nach** einer fehlgeschlagenen Nachprüfung (Bindung oder
//!    Zulässigkeit): [`AuditEvent::Denied`] — vor der Rückgabe an den
//!    Aufrufer, nicht danach.
//! 3. **Nach** dem Ausführungsversuch: [`AuditEvent::Executed`] oder
//!    [`AuditEvent::ExecutionFailed`], je nach Ergebnis.
//!
//! Ein Audit, das nur Erfolge kennt, ist kein Audit (Brief) — deshalb tragen
//! sowohl Ablehnung als auch fehlgeschlagene Ausführung ihren eigenen
//! Eintrag, und beide sind über die Tests
//! `test_denied_request_leaves_audit_entry` (`warden.rs`) und
//! `test_failed_execution_leaves_audit_entry` (`warden.rs`) belegt.
//!
//! # Warum `AuditEvent` Inhalt tragen darf, `WardenResponse` aber nicht
//! Das Audit-Protokoll verlässt den Vertrauensbereich des Warden **nicht**
//! — es ist für den Betreiber dieses Prozesses bestimmt, nicht für die
//! Gegenseite auf dem Socket. Die „inhaltsfrei"-Regel (Brief, Abschnitt
//! „Weitere Auflagen") gilt für das, was der Warden **antwortet**
//! ([`harw_dod_warden_proto::WardenResponse`] über
//! [`crate::warden::WardenOutcome::to_wire`]), nicht für das, was er
//! intern **aufzeichnet**. Ein Audit-Eintrag ohne die betroffene cgroup
//! oder den Ablehnungsgrund wäre für einen Betreiber wertlos.
//!
//! # Nebenläufigkeit
//! [`AuditSink`] verlangt nur `&self` für `record` — Implementierungen
//! müssen selbst für innere Synchronisation sorgen, wenn sie aus mehreren
//! Threads gleichzeitig aufgerufen werden. [`RecordingAuditSink`] tut das
//! über einen `Mutex<Vec<AuditEvent>>`.

use std::sync::Mutex;

use harw_dod_warden_proto::{Denial, WardenAction, WardenActionAudit};

/// Ein einzelner Audit-Eintrag.
///
/// # Description
/// Siehe Moduldoku für die Reihenfolge, in der [`crate::warden::Warden`]
/// diese Varianten erzeugt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditEvent {
    /// Eine nachgeprüfte, zulässige Aktion wird jetzt ausgeführt versucht.
    /// Aufgezeichnet **vor** dem Ausführungsversuch (siehe Moduldoku).
    Attempting {
        /// Die Aktion, deren Ausführung jetzt versucht wird.
        action: WardenAction,
    },
    /// Die Aktion wurde erfolgreich ausgeführt.
    Executed {
        /// Der Audit-Eintrag der ausgeführten Aktion (siehe
        /// [`WardenActionAudit`]).
        audit: WardenActionAudit,
    },
    /// Die Ausführung einer nachgeprüften, zulässigen Aktion ist
    /// fehlgeschlagen.
    ExecutionFailed {
        /// Die Aktion, deren Ausführung fehlgeschlagen ist.
        action: WardenAction,
    },
    /// Die Anfrage wurde abgelehnt, bevor irgendetwas ausgeführt wurde.
    Denied {
        /// Die abgelehnte Aktion.
        action: WardenAction,
        /// Die Ablehnungskategorie.
        reason: Denial,
    },
    /// Die Nachprüfung selbst konnte nicht durchgeführt werden (interne
    /// Kodierungspanne, siehe
    /// [`harw_dod_warden_proto::WardenProtoError::ActionEncoding`]) — keine
    /// geschäftslogische Ablehnung, sondern ein interner Defekt.
    VerificationFailed {
        /// Die Aktion, deren Nachprüfung nicht durchgeführt werden konnte.
        action: WardenAction,
    },
}

/// Nimmt Audit-Einträge entgegen.
///
/// # Description
/// Die einzige Nahtstelle zwischen [`crate::warden::Warden`] und einem
/// tatsächlichen Audit-Ziel (Datei, Syslog, ein anderer Prozess, ...). Diese
/// Crate liefert absichtlich nur [`RecordingAuditSink`] — eine
/// aufzeichnende Testimplementierung; ein echtes Ziel ist Sache des
/// künftigen Binaries (AW5-04b).
pub trait AuditSink {
    /// Zeichnet einen Audit-Eintrag auf.
    ///
    /// # Arguments
    /// - `event` (`AuditEvent`): der aufzuzeichnende Eintrag.
    fn record(&self, event: AuditEvent);
}

/// Aufzeichnende Testimplementierung von [`AuditSink`].
///
/// # Description
/// Sammelt jeden Aufruf von [`AuditSink::record`] in der Reihenfolge, in der
/// er geschah, hinter einem `Mutex`. Bestimmt für Tests — kein echtes
/// Audit-Ziel.
#[derive(Debug, Default)]
pub struct RecordingAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl RecordingAuditSink {
    /// Erzeugt eine leere Aufzeichnung.
    ///
    /// # Returns
    /// Einen [`RecordingAuditSink`] ohne Einträge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Gibt alle bisher aufgezeichneten Einträge zurück, in Aufrufreihenfolge.
    ///
    /// # Returns
    /// Eine Kopie der bisher aufgezeichneten Einträge.
    ///
    /// # Panics
    /// Wenn der interne Mutex vergiftet ist (ein vorheriger Aufruf ist über
    /// einen Panic ausgestiegen, während er den Mutex hielt) — für eine
    /// reine Testimplementierung akzeptabel.
    #[must_use]
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl AuditSink for RecordingAuditSink {
    fn record(&self, event: AuditEvent) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        events.push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::{AuditEvent, AuditSink, RecordingAuditSink};
    use crate::test_support::{TestResult, ctx};
    use harw_dod_warden_proto::{Denial, WardenAction};
    use harw_types::CgroupId;

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    #[test]
    fn test_new_sink_has_no_events() {
        let sink = RecordingAuditSink::new();
        assert!(sink.events().is_empty());
    }

    #[test]
    fn test_record_appends_in_call_order() -> TestResult {
        let sink = RecordingAuditSink::new();
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        sink.record(AuditEvent::Attempting {
            action: action.clone(),
        });
        sink.record(AuditEvent::Denied {
            action,
            reason: Denial::NotAdmissibleAtStage,
        });

        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AuditEvent::Attempting { .. }));
        assert!(matches!(events[1], AuditEvent::Denied { .. }));
        Ok(())
    }
}
