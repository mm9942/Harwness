//! Der echte Audit-Sink dieses Binaries: [`TracingAuditSink`].
//!
//! # Warum diese Crate ihn liefern muss
//! `harw-dod-warden` liefert bewusst nur `RecordingAuditSink` — laut dessen
//! eigener Moduldoku: „Bestimmt für Tests — kein echtes Audit-Ziel. Ein
//! echtes Ziel ist Sache des künftigen Binaries (AW5-04b)." Dieses Binary
//! ist genau das; [`TracingAuditSink`] ist die versprochene Umsetzung.
//!
//! # Warum `tracing`, keine zusätzliche Abhängigkeit
//! `tracing` ist ohnehin durch `--log` Pflicht (siehe `crate`-Moduldoku).
//! Ein Audit-Eintrag über `tracing::info!`/`warn!`/`error!` erreicht
//! denselben Betreiber-Kanal (z. B. `journalctl -f`), den jedes andere
//! Binary dieses Programms bereits nutzt — kein zusätzliches Blatt-Crate
//! (Datei-Sink, Netzwerk-Sink) nur für dieses eine Ziel, und ausdrücklich
//! **kein** Netz (siehe `crate`-Moduldoku, Abschnitt „Kein Netz").
//!
//! # Warum dieser Sink Inhalt tragen darf
//! Siehe `harw_dod_warden::audit`-Moduldoku, Abschnitt „Warum `AuditEvent`
//! Inhalt tragen darf, `WardenResponse` aber nicht": das Audit-Protokoll
//! verlässt den Vertrauensbereich des Warden nicht (im Unterschied zu
//! [`harw_dod_warden_proto::WardenResponse`] über den Socket, siehe
//! `crate::ipc`). Diese Implementierung protokolliert deshalb jedes Feld
//! strukturiert, statt es wie eine Wire-Antwort zu behandeln.
//!
//! # Nebenläufigkeit
//! [`TracingAuditSink`] trägt keinen inneren Zustand — `record` schreibt
//! ausschließlich an `tracing`, das selbst für Nebenläufigkeit sorgt. Sicher
//! aus mehreren Threads gleichzeitig aufrufbar (jeder von
//! [`crate::ipc::serve_forever`] gestartete Verbindungs-Thread ruft
//! potenziell gleichzeitig auf).

use harw_dod_warden::{AuditEvent, AuditSink};

/// Schreibt jeden Audit-Eintrag als strukturiertes `tracing`-Ereignis.
///
/// # Description
/// Siehe Moduldoku. Zustandslos — jede Instanz verhält sich identisch.
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingAuditSink;

impl TracingAuditSink {
    /// Baut einen neuen Sink.
    ///
    /// # Returns
    /// Einen [`TracingAuditSink`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl AuditSink for TracingAuditSink {
    /// Schreibt `event` als ein `tracing`-Ereignis passender Stufe.
    ///
    /// # Description
    /// Stufenwahl: [`AuditEvent::Attempting`]/[`AuditEvent::Executed`] als
    /// `info` (Regelbetrieb), [`AuditEvent::Denied`] als `warn`
    /// (geschäftslogische Ablehnung, kein Defekt), [`AuditEvent::ExecutionFailed`]/
    /// [`AuditEvent::VerificationFailed`] als `error` (ein Fehlschlag nach
    /// bereits erteilter Zulässigkeit bzw. eine interne Kodierungspanne).
    ///
    /// # Arguments
    /// - `event` (`AuditEvent`): der aufzuzeichnende Eintrag.
    fn record(&self, event: AuditEvent) {
        match event {
            AuditEvent::Attempting { action } => {
                tracing::info!(action = ?action, "warden attempting action");
            }
            AuditEvent::Executed { audit } => {
                tracing::info!(
                    audit_name = %audit.audit_name,
                    action = ?audit.action,
                    "warden executed action"
                );
            }
            AuditEvent::ExecutionFailed { action } => {
                tracing::error!(action = ?action, "warden action execution failed");
            }
            AuditEvent::Denied { action, reason } => {
                tracing::warn!(action = ?action, reason = %reason, "warden denied action");
            }
            AuditEvent::VerificationFailed { action } => {
                tracing::error!(
                    action = ?action,
                    "warden proof verification failed (internal encoding error, not a business denial)"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TracingAuditSink;
    use harw_dod_warden::{AuditEvent, AuditSink};
    use harw_dod_warden_proto::{Denial, WardenAction};
    use harw_types::CgroupId;

    fn cgroup(id: &str) -> CgroupId {
        CgroupId::try_from_str(id).expect("non-empty id")
    }

    #[test]
    fn test_record_does_not_panic_for_every_event_variant() {
        let sink = TracingAuditSink::new();
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1"),
        };
        sink.record(AuditEvent::Attempting {
            action: action.clone(),
        });
        sink.record(AuditEvent::Executed {
            audit: harw_dod_warden_proto::WardenActionAudit::for_action(action.clone()),
        });
        sink.record(AuditEvent::ExecutionFailed {
            action: action.clone(),
        });
        sink.record(AuditEvent::Denied {
            action: action.clone(),
            reason: Denial::ProofMismatch,
        });
        sink.record(AuditEvent::VerificationFailed { action });
    }
}
