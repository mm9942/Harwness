//! Das Audit-Protokoll: [`AuditEvent`], [`AuditSink`], [`RecordingAuditSink`].
//!
//! # Verantwortungsbereich
//! „Was protokolliert werden soll, wird protokolliert, **bevor** die Aktion
//! versucht wird" (Brief). [`crate::warden::Warden::handle`] ruft
//! [`AuditSink::record`] deshalb an drei Stellen auf, in dieser Reihenfolge:
//!
//! 1. **Vor** jedem Ausführungsversuch: [`AuditEvent::Attempting`] — erst,
//!    nachdem der dauerhafte `attempt`-Eintrag im Aktions-Ledger
//!    (`action_ledger.rs`) geschrieben ist; auch wenn die anschließende
//!    Ausführung fehlschlägt, existiert bereits ein Eintrag, der die
//!    versuchte Aktion nennt.
//! 2. **Nach** einer fehlgeschlagenen Nachprüfung (Autorisierung, Policy,
//!    Zielprüfung): [`AuditEvent::Denied`] — vor der Rückgabe an den
//!    Aufrufer, nicht danach.
//! 3. **Nach** dem Ausführungsversuch: [`AuditEvent::Executed`] oder
//!    [`AuditEvent::ExecutionFailed`], je nach Ergebnis.
//!
//! Dazu kommen [`AuditEvent::HoldArmed`], [`AuditEvent::HoldDropped`] und
//! [`AuditEvent::HoldAbandoned`] für
//! den Hold-Timer und [`AuditEvent::Unavailable`], wenn der Warden fail
//! closed nichts tun konnte (Sperre vergiftet, Ledger oder Hold-Speicher
//! nicht schreibbar).
//!
//! Ein Audit, das nur Erfolge kennt, ist kein Audit (Brief) — deshalb tragen
//! sowohl Ablehnung als auch fehlgeschlagene Ausführung ihren eigenen
//! Eintrag (Tests in `warden.rs`).
//!
//! Der [`AuditSink`] ist die Betreiber-Sicht (Journal); das dauerhafte,
//! hash-verkettete Protokoll ist das Aktions-Ledger.
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

use std::sync::{Arc, Mutex};

use harw_dod_warden_proto::{Denial, WardenAction, WardenActionAudit};
use harw_types::CgroupId;
use jiff::Timestamp;

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
    /// Der Warden konnte fail closed nicht arbeiten (Sperre vergiftet,
    /// Aktions-Ledger oder Hold-Speicher nicht schreibbar) — kein
    /// geschäftslogischer Ablehnungsgrund, sondern ein interner Defekt.
    Unavailable {
        /// Die betroffene Aktion.
        action: WardenAction,
        /// Fester Grund (kein Pfad, kein Inhalt).
        reason: &'static str,
    },
    /// Ein Freeze mit Haltedauer hat einen Hold-Timer gestellt; er taut bei
    /// Ablauf nur auf.
    HoldArmed {
        /// Die eingefrorene cgroup.
        cgroup: CgroupId,
        /// Fälligkeit des Auftauens.
        thaw_at: Timestamp,
    },
    /// Ein fälliger Hold wurde verworfen, weil das Ziel nicht mehr dasselbe
    /// ist (verschwunden oder Kernel-ID geändert); nichts wurde aufgetaut.
    HoldDropped {
        /// Die cgroup des verworfenen Holds.
        cgroup: CgroupId,
    },
    /// Das Auftauen eines fälligen Holds ist wiederholt gescheitert und wurde
    /// aufgegeben; die cgroup bleibt eingefroren, bis ein signierter Thaw sie
    /// freigibt.
    HoldAbandoned {
        /// Die cgroup des aufgegebenen Holds.
        cgroup: CgroupId,
        /// Zahl der fehlgeschlagenen Auftau-Versuche.
        attempts: u32,
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
/// er geschah, hinter einem `Mutex`. Klone teilen dieselbe Aufzeichnung
/// (`Arc`), sodass ein Test einen Klon an den Warden geben und die Einträge
/// danach lesen kann. Bestimmt für Tests — kein echtes Audit-Ziel.
#[derive(Debug, Default, Clone)]
pub struct RecordingAuditSink {
    events: Arc<Mutex<Vec<AuditEvent>>>,
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
    /// Eine Kopie der bisher aufgezeichneten Einträge (geteilt über alle
    /// Klone). Ein vergifteter Mutex wird übernommen — für eine reine
    /// Testimplementierung akzeptabel.
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
        assert!(matches!(
            events.first(),
            Some(AuditEvent::Attempting { .. })
        ));
        assert!(matches!(events.get(1), Some(AuditEvent::Denied { .. })));
        Ok(())
    }

    #[test]
    fn test_clones_share_one_recording() -> TestResult {
        let sink = RecordingAuditSink::new();
        let clone = sink.clone();
        clone.record(AuditEvent::HoldDropped {
            cgroup: cgroup("harw.slice/job-1")?,
        });
        assert_eq!(sink.events().len(), 1);
        Ok(())
    }
}
