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
//! strukturiert, statt es wie eine Wire-Antwort zu behandeln — mit einer
//! Grenze (PL-100 Journal-Hygiene): Ereignisse auf `warn` und `error`
//! nennen nur Aktionsart und festen Grund; der cgroup-Pfad steht auf `info`
//! bzw. in einem eigenen `debug`-Ereignis. Das dauerhafte, vollständige
//! Protokoll ist das hash-verkettete Aktions-Ledger im StateDirectory.
//!
//! # Ungeprüfte Pfade nur escapet
//! Eine Ablehnung kann vor der Nachprüfung entstehen; ihr cgroup-Pfad ist
//! dann vom Peer frei gewählt (`CgroupId` garantiert nur „nicht leer“) und
//! darf Steuerzeichen oder Zeilenumbrüche enthalten. Die `debug`-Ereignisse
//! zu [`AuditEvent::Denied`], [`AuditEvent::Unavailable`],
//! [`AuditEvent::ExecutionFailed`] und den Hold-Ereignissen loggen den Pfad
//! deshalb mit `Debug` (`?`), das Steuerzeichen escapet. Die `info`-Ereignisse
//! betreffen nur nachgeprüfte Aktionen, deren Pfad die Deny-Liste als
//! wohlgeformt angenommen hat.
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
    /// Stufenwahl: [`AuditEvent::Attempting`]/[`AuditEvent::Executed`]/
    /// [`AuditEvent::HoldArmed`] als `info` (Regelbetrieb, mit cgroup-Pfad),
    /// [`AuditEvent::ContainmentCleared`]/[`AuditEvent::ContainmentRestored`]
    /// als `info` (Kernel-ID und fester Grund; der Pfad auf `debug`),
    /// [`AuditEvent::Denied`]/[`AuditEvent::HoldDropped`]/
    /// [`AuditEvent::ContainmentDegraded`]/[`AuditEvent::ContainmentCompleted`]/
    /// [`AuditEvent::ContainmentUnresolved`] als `warn`,
    /// [`AuditEvent::ExecutionFailed`]/[`AuditEvent::Unavailable`]/
    /// [`AuditEvent::HoldAbandoned`] als `error`. Auf `warn` und höher stehen nur die Aktionsart und feste
    /// Gründe — der cgroup-Pfad folgt in einem eigenen `debug`-Ereignis
    /// (keine Pfade ab `warn`), dort mit `Debug` escapet (siehe Moduldoku).
    ///
    /// # Arguments
    /// - `event` (`AuditEvent`): der aufzuzeichnende Eintrag.
    fn record(&self, event: AuditEvent) {
        match event {
            AuditEvent::Attempting { action } => {
                tracing::info!(
                    kind = action.kind_name(),
                    cgroup = %action.cgroup(),
                    "warden attempting action"
                );
            }
            AuditEvent::Executed { audit } => {
                tracing::info!(
                    audit_name = %audit.audit_name,
                    cgroup = %audit.action.cgroup(),
                    "warden executed action"
                );
            }
            AuditEvent::ExecutionFailed { action } => {
                tracing::error!(kind = action.kind_name(), "warden action execution failed");
                tracing::debug!(cgroup = ?action.cgroup(), "failed action target");
            }
            AuditEvent::Denied { action, reason } => {
                tracing::warn!(kind = action.kind_name(), reason = %reason, "warden denied action");
                tracing::debug!(cgroup = ?action.cgroup(), "denied action target");
            }
            AuditEvent::Unavailable { action, reason } => {
                tracing::error!(
                    kind = action.kind_name(),
                    reason,
                    "warden could not act (fail closed)"
                );
                tracing::debug!(cgroup = ?action.cgroup(), "unavailable action target");
            }
            AuditEvent::HoldArmed { cgroup, thaw_at } => {
                tracing::info!(cgroup = %cgroup, thaw_at = %thaw_at, "warden hold armed; expiry only thaws");
            }
            AuditEvent::HoldDropped { cgroup } => {
                tracing::warn!(
                    "warden dropped a due hold: target changed or not contained by the warden in this boot, nothing thawed"
                );
                tracing::debug!(cgroup = ?cgroup, "dropped hold target");
            }
            AuditEvent::HoldAbandoned { cgroup, attempts } => {
                tracing::error!(
                    attempts,
                    "warden gave up thawing a due hold; the cgroup stays frozen until a signed thaw"
                );
                tracing::debug!(cgroup = ?cgroup, "abandoned hold target");
            }
            AuditEvent::ContainmentDegraded { action, reason } => {
                // Ob die Aktion selbst gelang, sagen `Executed` bzw.
                // `ExecutionFailed`; hier nur der Schritt des Protokolls.
                tracing::warn!(
                    kind = action.kind_name(),
                    reason,
                    "warden containment marker protocol step failed"
                );
                tracing::debug!(cgroup = ?action.cgroup(), "degraded containment target");
            }
            AuditEvent::ContainmentCleared {
                cgroup,
                kernel_id,
                reason,
            } => {
                tracing::info!(
                    kernel_id = kernel_id.get(),
                    reason,
                    "warden cleared a stale containment marker"
                );
                tracing::debug!(cgroup = ?cgroup, "cleared containment target");
            }
            AuditEvent::ContainmentRestored { cgroup, kernel_id } => {
                tracing::info!(
                    kernel_id = kernel_id.get(),
                    "warden restored the missing containment marker of a cgroup that is still frozen"
                );
                tracing::debug!(cgroup = ?cgroup, "restored containment target");
            }
            AuditEvent::ContainmentCompleted { cgroup, kernel_id } => {
                tracing::warn!(
                    kernel_id = kernel_id.get(),
                    "warden completed an unfinished containment of a frozen cgroup; its freeze bit belongs to another owner and no warden release lifts it"
                );
                tracing::debug!(cgroup = ?cgroup, "completed containment target");
            }
            AuditEvent::ContainmentUnresolved {
                cgroup,
                kernel_id,
                reason,
            } => {
                tracing::warn!(
                    kernel_id = kernel_id.get(),
                    reason,
                    "warden left a containment unresolved; a leftover marker keeps job.resume refused until a signed thaw clears it"
                );
                tracing::debug!(cgroup = ?cgroup, "unresolved containment target");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::TracingAuditSink;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden::{AuditEvent, AuditSink};
    use harw_dod_warden_proto::{Denial, WardenAction};
    use harw_types::CgroupId;

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    // Fängt die formatierte `tracing`-Ausgabe für eine Prüfung ein.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("capture poisoned"))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Capture {
        fn text(&self) -> TestResult<String> {
            let bytes = self
                .0
                .lock()
                .map_err(|_| TestError::Missing("capture lock"))?
                .clone();
            String::from_utf8(bytes).map_err(ctx("utf-8 log output"))
        }
    }

    #[test]
    fn test_unverified_cgroup_paths_are_escaped_in_debug_lines() -> TestResult {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        // Ein vom Peer frei gewählter Pfad mit Zeilenumbruch: er darf keine
        // eigene, gefälschte Journal-Zeile erzeugen.
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/evil\nINFO forged entry")?,
        };
        tracing::subscriber::with_default(subscriber, || {
            TracingAuditSink::new().record(AuditEvent::Denied {
                action,
                reason: Denial::ProofMismatch,
            });
        });
        let text = capture.text()?;
        let line = text
            .lines()
            .find(|line| line.contains("denied action target"))
            .ok_or(TestError::Missing("debug line with the target"))?;
        assert!(line.contains("forged entry"), "{text}");
        assert!(
            !text.lines().any(|line| line.starts_with("INFO forged")),
            "{text}"
        );
        Ok(())
    }

    #[test]
    fn test_record_does_not_panic_for_every_event_variant() -> TestResult {
        let sink = TracingAuditSink::new();
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
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
        sink.record(AuditEvent::Unavailable {
            action: action.clone(),
            reason: "action ledger not writable; nothing executed",
        });
        sink.record(AuditEvent::HoldArmed {
            cgroup: action.cgroup().clone(),
            thaw_at: jiff::Timestamp::UNIX_EPOCH,
        });
        sink.record(AuditEvent::HoldDropped {
            cgroup: action.cgroup().clone(),
        });
        sink.record(AuditEvent::HoldAbandoned {
            cgroup: action.cgroup().clone(),
            attempts: 8,
        });
        sink.record(AuditEvent::ContainmentDegraded {
            action: action.clone(),
            reason: "containment index lock not obtained; containing without it",
        });
        sink.record(AuditEvent::ContainmentCleared {
            cgroup: action.cgroup().clone(),
            kernel_id: harw_dod_warden_proto::CgroupKernelId::new(4242).map_err(ctx("kid"))?,
            reason: "hold of a cgroup that no longer exists dropped",
        });
        sink.record(AuditEvent::ContainmentRestored {
            cgroup: action.cgroup().clone(),
            kernel_id: harw_dod_warden_proto::CgroupKernelId::new(4242).map_err(ctx("kid"))?,
        });
        sink.record(AuditEvent::ContainmentCompleted {
            cgroup: action.cgroup().clone(),
            kernel_id: harw_dod_warden_proto::CgroupKernelId::new(4242).map_err(ctx("kid"))?,
        });
        sink.record(AuditEvent::ContainmentUnresolved {
            cgroup: None,
            kernel_id: harw_dod_warden_proto::CgroupKernelId::new(4242).map_err(ctx("kid"))?,
            reason: "marker without a containment record kept (fail closed)",
        });
        Ok(())
    }

    #[test]
    fn test_containment_warnings_name_no_cgroup_path() -> TestResult {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/very-identifiable-job")?,
        };
        tracing::subscriber::with_default(subscriber, || {
            TracingAuditSink::new().record(AuditEvent::ContainmentDegraded {
                action,
                reason: "containment marker not created; freezing without it",
            });
        });
        let text = capture.text()?;
        assert!(text.contains("containment marker not created"), "{text}");
        // Neutral: die Meldung behauptet nicht, die Aktion sei weitergelaufen.
        assert!(text.contains("protocol step failed"), "{text}");
        assert!(!text.contains("went on"), "{text}");
        assert!(!text.contains("very-identifiable"), "{text}");
        Ok(())
    }

    // Auch die Meldungen des Abgleichs beim Start nennen ab `info` nur die
    // Kernel-ID und den festen Grund, nie den cgroup-Pfad.
    #[test]
    fn test_reconciliation_events_name_no_cgroup_path() -> TestResult {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let path = cgroup("harw.slice/very-identifiable-job")?;
        let kernel_id = harw_dod_warden_proto::CgroupKernelId::new(4242).map_err(ctx("kid"))?;
        tracing::subscriber::with_default(subscriber, || {
            let sink = TracingAuditSink::new();
            sink.record(AuditEvent::ContainmentUnresolved {
                cgroup: Some(path.clone()),
                kernel_id,
                reason: "cgroup.freeze unreadable; containment kept",
            });
            sink.record(AuditEvent::ContainmentRestored {
                cgroup: path.clone(),
                kernel_id,
            });
            sink.record(AuditEvent::ContainmentCompleted {
                cgroup: path.clone(),
                kernel_id,
            });
            sink.record(AuditEvent::ContainmentCleared {
                cgroup: path,
                kernel_id,
                reason: "startup reconciliation: the cgroup no longer exists or was replaced",
            });
        });
        let text = capture.text()?;
        assert!(text.contains("cgroup.freeze unreadable"), "{text}");
        assert!(text.contains("unfinished containment"), "{text}");
        assert!(text.contains("4242"), "{text}");
        assert!(text.contains("WARN"), "{text}");
        assert!(!text.contains("very-identifiable"), "{text}");
        Ok(())
    }
}
