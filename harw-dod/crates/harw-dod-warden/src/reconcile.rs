//! Abgleich der Eindämmungen beim Start und im laufenden Betrieb:
//! [`ContainmentReconciliation`] (PL-100, Owner-Review 4505015305:
//! `job.resume` darf nie einen Freeze des Wardens aufheben).
//!
//! # Zweck
//! Die Marker im Indexverzeichnis (`containment.rs`) überleben einen Neustart
//! des Wardens: tmpfiles legt das Verzeichnis unter `/run` an, kein
//! `RuntimeDirectory=` räumt es ab. Die Datensätze im StateDirectory
//! überleben sogar einen Neustart des Hosts. Stürzt der Warden zwischen zwei
//! Schritten des Marker-Protokolls ab (etwa nach `cgroup.freeze` = 0 und vor
//! dem Entfernen des Markers), bliebe ein Marker liegen, und harw verweigerte
//! `job.resume` für immer. [`crate::warden::Warden::reconcile_containments`]
//! gleicht deshalb vor der ersten Anfrage Datensätze, Marker, Holds und die
//! cgroups selbst ab. Er schreibt nie `cgroup.freeze`, friert nichts ein und
//! taut nichts auf; er entfernt oder ergänzt nur Marker und Datensätze.
//!
//! # Regeln je Datensatz beim Start
//! 0. **Aus einem früheren Boot** (`containment.rs`, „Boot-ID“; ebenso, wenn
//!    sich derselbe Boot nicht nachweisen lässt, weil eine der beiden
//!    Boot-IDs unbekannt ist): verwerfen und melden; die Kernel-ID kann jetzt
//!    eine andere cgroup bezeichnen. Einen
//!    Marker gleichen Namens fasst der Abgleich nicht an (nach einem Boot ist
//!    `/run` leer; ein Marker wäre neu und fällt unter „Marker ohne
//!    Datensatz“). Gezählt unter [`ContainmentReconciliation::vanished`].
//! 1. **Schon freigegeben** (`released`: die Freigabe lief und hat das vor
//!    dem Entfernen des Markers dauerhaft gebucht; der Marker ließ sich nicht
//!    entfernen, oder der Warden stürzte vor dem Entfernen des Datensatzes
//!    ab): erst den Marker (falls noch da), dann den Datensatz entfernen
//!    ([`ContainmentReconciliation::released`]). Nie wird ein Marker neu
//!    angelegt, auch wenn die cgroup noch eingefroren ist (das Bit gehört
//!    dann einem anderen, etwa harws `job.suspend`).
//!
//! Für alle anderen wird die cgroup unter dem aufgezeichneten Pfad beobachtet
//! ([`TargetObserver`]) und das eigene Bit über den gepinnten Deskriptor
//! gelesen ([`CgroupFreezer::freeze_requested`]):
//! 2. **Verschwunden oder ersetzt** (keine cgroup unter dem Pfad oder eine
//!    andere Kernel-ID): erst den Marker, dann den Datensatz entfernen
//!    ([`ContainmentReconciliation::vanished`]).
//! 3. **Lebt, `cgroup.freeze` = 0, kein Hold derselben cgroup** (Pfad und
//!    Kernel-ID): Absturz zwischen Auftauen und Entfernen, oder vor dem
//!    Einfrieren (Datensatz ohne `freeze_was_set`). Erst den Marker, dann den
//!    Datensatz entfernen ([`ContainmentReconciliation::thawed`]).
//! 4. **Lebt, `cgroup.freeze` = 0, Hold läuft**: bleibt; der Hold-Timer gibt
//!    die Eindämmung frei ([`ContainmentReconciliation::kept`]).
//! 5. **Lebt, `cgroup.freeze` = 1**: bleibt, und ihr Marker muss da sein
//!    (Eindämmung geht vor). Fehlt er (etwa weil das Indexverzeichnis beim
//!    Freeze nicht nutzbar war), wird er neu angelegt
//!    ([`ContainmentReconciliation::restored`]). Ein nicht abgeschlossener
//!    Datensatz wird vorher mit `freeze_was_set` = `true` abgeschlossen
//!    ([`ContainmentReconciliation::completed`]): `freeze_was_set` steht
//!    dauerhaft fest, bevor der Warden `cgroup.freeze` = 1 schreibt; fehlt
//!    es, hat ein anderer (etwa harws `job.suspend`) das Bit gesetzt, und
//!    keine Freigabe des Wardens darf es aufheben. Lässt sich
//!    `freeze_was_set` beim Freeze nicht dauerhaft schreiben, friert der
//!    Warden gar nicht ein (`Unavailable`); ein nicht abgeschlossener
//!    Datensatz bei gesetztem Bit stammt also nie von einem eigenen Freeze.
//! 6. **Nicht entscheidbar** (Beobachtung oder Lesen scheitert anders, oder
//!    der Marker lässt sich nicht entfernen bzw. nicht anlegen): bleibt
//!    unverändert ([`ContainmentReconciliation::unresolved`]).
//!
//! # Marker ohne Datensatz
//! Ein Marker, zu dem kein Datensatz gehört (der Freeze stellt den Datensatz
//! vor dem Marker, siehe `containment.rs`; übrig bleibt so ein Marker nur,
//! wenn ein Entfernen scheiterte oder der Zustand verloren ging), nennt keinen
//! Pfad, unter dem der Warden die cgroup prüfen könnte. Er bleibt (fail
//! closed: vielleicht gehört er zu einer eingefrorenen cgroup) und wird
//! gemeldet ([`ContainmentReconciliation::orphaned`]); ein signierter Thaw
//! dieser cgroup räumt ihn ab (`warden.rs`, Operator-Pfad).
//!
//! # Im laufenden Betrieb
//! Der Hold-Timer ruft regelmäßig den Durchlauf `Reconciler::sweep` auf
//! (`warden.rs`, `Warden::expire_holds`): er entfernt Marker und Datensatz
//! jeder Eindämmung, deren cgroup verschwunden oder ersetzt ist (etwa nach
//! einem Kill: `cgroup.kill` lässt das Verzeichnis stehen, systemd entfernt
//! es erst später), und jeder schon freigegebenen, deren Marker sich zunächst
//! nicht entfernen ließ. Eine nur im Speicher gebuchte Freigabe schreibt er
//! zuerst nach (`ContainmentStore::flush`); gelingt das nicht, entfernt er
//! den Marker einer freigegebenen Eindämmung nur, wenn ihre cgroup
//! verschwunden oder ersetzt ist (ein Absturz brächte sonst den Marker einer
//! eingefrorenen cgroup zurück). Er liest kein Bit und fasst lebende
//! Eindämmungen nicht an. Ein Marker, der sich wieder nicht entfernen lässt,
//! bleibt still stehen (gemeldet wurde er schon beim ersten Versuch) und
//! wird beim nächsten Durchlauf erneut versucht.
//!
//! # Keine Symlinks
//! Das Indexverzeichnis wird mit `O_NOFOLLOW` geöffnet und muss dem Warden
//! gehören ([`ContainmentMarkers::open_locked`]); sonst scheitert der Abgleich
//! und das Binary startet nicht (der Durchlauf im Betrieb tut dann nichts).
//! Einträge werden über `getdents64` und `fstatat` ohne Symlink-Folgen
//! geprüft ([`crate::containment::MarkerDir::scan`]); entfernt wird nur eine
//! reguläre Datei (`unlinkat` folgt nie einem Link). Ein Symlink, ein
//! Verzeichnis oder ein fremder Name bleibt unberührt
//! ([`ContainmentReconciliation::foreign`]).
//!
//! # Sperre
//! Wie beim Eindämmen genau ein Versuch `flock(LOCK_EX | LOCK_NB)` auf das
//! Indexverzeichnis; ohne Sperre läuft der Abgleich trotzdem
//! ([`ContainmentReconciliation::index_locked`]). Der Durchlauf im Betrieb
//! entfernt nur Marker und sperrt nicht (wie das Freigeben).
//!
//! # Audit
//! Jedes Entfernen meldet [`AuditEvent::ContainmentCleared`], jeder neu
//! angelegte Marker [`AuditEvent::ContainmentRestored`], jeder
//! abgeschlossene Datensatz [`AuditEvent::ContainmentCompleted`], jede stehen
//! gelassene, nicht geklärte Eindämmung [`AuditEvent::ContainmentUnresolved`].
//! Das Aktions-Ledger bleibt unberührt: weder Abgleich noch Durchlauf greifen
//! in eine cgroup ein.
//!
//! # Nebenläufigkeit
//! Läuft unter der Eingriffssperre des Wardens; das Binary ruft den Abgleich
//! nach Landlock und seccomp (so prüft schon der Start die erlaubten Pfade
//! und Aufrufe) und vor dem Hold-Timer und der ersten Anfrage auf.
//!
//! # Fehler
//! [`crate::error::WardenError::UntrustedFile`] und
//! [`crate::error::WardenError::Io`] für ein unbrauchbares Indexverzeichnis,
//! [`crate::error::WardenError::StatePoisoned`] für einen vergifteten
//! Speicher. Alles, was nur einen Datensatz oder Marker betrifft,
//! ist kein Fehler, sondern ein Zähler und ein Audit-Eintrag.
//!
//! # Examples
//! ```rust,ignore
//! // Im Binary, nach Landlock und seccomp:
//! let report = warden.reconcile_containments()?;
//! tracing::info!(kept = report.kept, vanished = report.vanished, "containments reconciled");
//! ```

use std::collections::BTreeSet;

use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget};

use crate::audit::{AuditEvent, AuditSink};
use crate::containment::{
    ContainmentEntry, ContainmentMarkers, ContainmentStore, MarkerCreation, MarkerDir,
};
use crate::error::{WardenError, WardenResult};
use crate::executor::CgroupFreezer;
use crate::holds::{HoldEntry, HoldStore};
use crate::observe::TargetObserver;

// Feste Gründe (kein Pfad, kein Inhalt).
const CLEARED_PREVIOUS_BOOT: &str = "startup reconciliation: record from a previous boot (or the same boot is not provable); kernel ids are not comparable across boots";
const CLEARED_RELEASED: &str =
    "startup reconciliation: leftover marker of an already released containment removed";
const CLEARED_VANISHED: &str =
    "startup reconciliation: the cgroup no longer exists or was replaced";
const CLEARED_THAWED: &str =
    "startup reconciliation: the cgroup is no longer frozen and no hold is running";
const SWEPT_VANISHED: &str = "hold timer sweep: the cgroup no longer exists or was replaced";
const SWEPT_RELEASED: &str =
    "hold timer sweep: leftover marker of an already released containment removed";
const UNRESOLVED_UNOBSERVABLE: &str = "cgroup could not be observed; containment kept";
const UNRESOLVED_UNREADABLE: &str = "cgroup.freeze unreadable; containment kept";
const UNRESOLVED_NOT_REMOVED: &str =
    "stale marker not removable (not a regular file or i/o error); containment kept";
const UNRESOLVED_NOT_A_MARKER: &str =
    "entry under the marker name of a frozen containment is not a regular file";
const UNRESOLVED_NOT_RESTORED: &str = "marker of a frozen containment missing and not restorable";
const UNRESOLVED_STORE: &str =
    "containment store not writable; stale record removed in memory only";
const UNRESOLVED_NOT_COMPLETED: &str =
    "containment store not writable; unfinished containment completed in memory only";
const UNRESOLVED_ORPHAN: &str =
    "marker without a containment record kept (fail closed); a signed thaw of its cgroup clears it";

/// Ergebnis von [`crate::warden::Warden::reconcile_containments`].
///
/// # Description
/// Jeder Datensatz dieses Boots zählt genau einmal in [`Self::kept`],
/// [`Self::vanished`], [`Self::thawed`], [`Self::released`] oder
/// [`Self::unresolved`]; Datensätze eines früheren Boots zählen in
/// [`Self::vanished`]. [`Self::restored`] und [`Self::completed`] sind
/// Teilmengen von [`Self::kept`]. Marker ohne Datensatz zählen in
/// [`Self::orphaned`], fremde Einträge des Indexverzeichnisses in
/// [`Self::foreign`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContainmentReconciliation {
    /// Ob der Abgleich die exklusive Sperre des Indexverzeichnisses hielt.
    pub index_locked: bool,
    /// Eindämmungen, die bleiben: cgroup eingefroren, oder ein Hold läuft.
    pub kept: usize,
    /// Davon: der fehlende Marker einer eingefrorenen cgroup wurde neu
    /// angelegt.
    pub restored: usize,
    /// Davon: ein nicht abgeschlossener Datensatz einer eingefrorenen cgroup
    /// wurde mit `freeze_was_set` = `true` abgeschlossen (das Bit gehört
    /// einem anderen).
    pub completed: usize,
    /// Abgeräumt: die cgroup ist verschwunden oder ersetzt, oder der
    /// Datensatz stammt aus einem früheren Boot.
    pub vanished: usize,
    /// Abgeräumt: die cgroup ist nicht mehr eingefroren, und kein Hold läuft.
    pub thawed: usize,
    /// Abgeräumt: die Eindämmung war schon freigegeben, nur ihr Marker lag
    /// noch.
    pub released: usize,
    /// Unverändert gelassen, weil Beobachtung, Lesen, Entfernen oder Anlegen
    /// scheiterte (siehe [`AuditEvent::ContainmentUnresolved`]).
    pub unresolved: usize,
    /// Marker ohne Datensatz; sie bleiben (fail closed).
    pub orphaned: usize,
    /// Einträge des Indexverzeichnisses, die keine Marker sind (fremder Name,
    /// Symlink, Verzeichnis); nie angefasst.
    pub foreign: usize,
    /// Ob das Indexverzeichnis mehr Einträge hatte, als gelesen wurden
    /// ([`crate::containment::MAX_INDEX_ENTRIES`]).
    pub truncated: bool,
}

// Ausgang eines einzelnen Datensatzes.
enum RecordFate {
    Kept { restored: bool, completed: bool },
    Vanished,
    Thawed,
    Released,
    Unresolved,
}

// Die Bausteine des Abgleichs (aus dem Warden geliehen).
pub(crate) struct Reconciler<'a> {
    pub(crate) markers: &'a ContainmentMarkers,
    pub(crate) containment: &'a ContainmentStore,
    pub(crate) holds: &'a HoldStore,
    pub(crate) observer: &'a (dyn TargetObserver + Send + Sync),
    pub(crate) freezer: &'a (dyn CgroupFreezer + Send + Sync),
    pub(crate) audit: &'a (dyn AuditSink + Send + Sync),
}

impl Reconciler<'_> {
    // Ein Durchlauf beim Start: Indexverzeichnis prüfen und sperren,
    // Datensätze eines früheren Boots verwerfen, jeden Datensatz abgleichen,
    // dann die Marker ohne Datensatz melden. Die Sperre fällt mit dem
    // Verzeichnis am Ende.
    pub(crate) fn run(&self) -> WardenResult<ContainmentReconciliation> {
        let dir = self.markers.open_locked()?;
        let mut report = ContainmentReconciliation {
            index_locked: dir.is_locked(),
            ..ContainmentReconciliation::default()
        };
        report.vanished = self.discard_previous_boot()?;
        let records = self.containment.snapshot()?;
        let holds = self.holds.snapshot()?;
        for entry in &records {
            match self.reconcile_record(&dir, entry, &holds) {
                RecordFate::Kept {
                    restored,
                    completed,
                } => {
                    report.kept = report.kept.saturating_add(1);
                    if restored {
                        report.restored = report.restored.saturating_add(1);
                    }
                    if completed {
                        report.completed = report.completed.saturating_add(1);
                    }
                }
                RecordFate::Vanished => report.vanished = report.vanished.saturating_add(1),
                RecordFate::Thawed => report.thawed = report.thawed.saturating_add(1),
                RecordFate::Released => report.released = report.released.saturating_add(1),
                RecordFate::Unresolved => {
                    report.unresolved = report.unresolved.saturating_add(1);
                }
            }
        }
        let recorded: BTreeSet<CgroupKernelId> =
            records.iter().map(|entry| entry.target.kernel_id).collect();
        let scan = dir.scan()?;
        report.foreign = scan.foreign;
        report.truncated = scan.truncated;
        for kernel_id in scan.markers {
            if recorded.contains(&kernel_id) {
                continue;
            }
            report.orphaned = report.orphaned.saturating_add(1);
            self.audit.record(AuditEvent::ContainmentUnresolved {
                cgroup: None,
                kernel_id,
                reason: UNRESOLVED_ORPHAN,
            });
        }
        drop(dir);
        Ok(report)
    }

    // Ein Durchlauf im laufenden Betrieb (Hold-Timer): Marker und Datensatz
    // jeder Eindämmung entfernen, deren cgroup verschwunden oder ersetzt ist,
    // und jeder dauerhaft freigegebenen. Liest kein Bit, fasst lebende
    // Eindämmungen nicht an. Gibt zurück, wie viele abgeräumt wurden.
    pub(crate) fn sweep(&self) -> usize {
        // Einen nur im Speicher gebuchten Stand zuerst nachschreiben (auch
        // ohne nutzbaren Index): erst eine dauerhafte Freigabe nimmt den
        // Marker einer lebenden cgroup weg (ein Absturz brächte ihn sonst
        // zurück, `containment.rs`).
        let released_durably = self.containment.flush().is_ok();
        let dir = match self.markers.open() {
            Ok(dir) => Some(dir),
            // Ohne Indexverzeichnis gibt es keinen Marker, der bleiben müsste.
            Err(WardenError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            // Ein unbrauchbares Indexverzeichnis (Symlink, fremder
            // Eigentümer): nichts anfassen, auch keinen Datensatz.
            Err(_) => return 0,
        };
        let Ok(records) = self.containment.snapshot() else {
            return 0;
        };
        let mut cleared: Vec<(&CgroupTarget, &'static str)> = Vec::new();
        for entry in &records {
            let target = &entry.target;
            let reason = if entry.released && released_durably {
                SWEPT_RELEASED
            } else {
                match self.observer.kernel_id(&target.path) {
                    Ok(observed) if observed == target.kernel_id => continue,
                    Ok(_) => SWEPT_VANISHED,
                    Err(error) if error.is_target_gone() => SWEPT_VANISHED,
                    Err(_) => continue,
                }
            };
            let removed = match dir.as_ref() {
                Some(dir) => dir.remove(target.kernel_id),
                None => Ok(false),
            };
            // Lässt sich der Marker wieder nicht entfernen, bleibt auch der
            // Datensatz; gemeldet wurde das beim ersten Versuch.
            if removed.is_ok() {
                cleared.push((target, reason));
            }
        }
        if cleared.is_empty() {
            return 0;
        }
        let kernel_ids: Vec<CgroupKernelId> =
            cleared.iter().map(|(target, _)| target.kernel_id).collect();
        let written = self.containment.forget_many(&kernel_ids).is_ok();
        for &(target, reason) in &cleared {
            if !written {
                self.store_not_written(target);
            }
            self.audit.record(AuditEvent::ContainmentCleared {
                cgroup: target.path.clone(),
                kernel_id: target.kernel_id,
                reason,
            });
        }
        cleared.len()
    }

    // Regel 0: Datensätze eines früheren Boots verwerfen (Marker unberührt)
    // und melden. Gibt ihre Zahl zurück.
    fn discard_previous_boot(&self) -> WardenResult<usize> {
        let previous = self.containment.previous_boot()?;
        if previous.is_empty() {
            return Ok(0);
        }
        let written = self.containment.discard_previous_boot().is_ok();
        for entry in &previous {
            if !written {
                self.store_not_written(&entry.target);
            }
            self.audit.record(AuditEvent::ContainmentCleared {
                cgroup: entry.target.path.clone(),
                kernel_id: entry.target.kernel_id,
                reason: CLEARED_PREVIOUS_BOOT,
            });
        }
        Ok(previous.len())
    }

    // Regeln 1 bis 6 der Moduldoku für einen Datensatz.
    fn reconcile_record(
        &self,
        dir: &MarkerDir,
        entry: &ContainmentEntry,
        holds: &[HoldEntry],
    ) -> RecordFate {
        let target = &entry.target;
        if entry.released {
            return self.clear(dir, target, CLEARED_RELEASED, RecordFate::Released);
        }
        match self.observer.kernel_id(&target.path) {
            Ok(observed) if observed == target.kernel_id => {}
            Ok(_) => return self.clear(dir, target, CLEARED_VANISHED, RecordFate::Vanished),
            Err(error) if error.is_target_gone() => {
                return self.clear(dir, target, CLEARED_VANISHED, RecordFate::Vanished);
            }
            Err(_) => return self.unresolved(target, UNRESOLVED_UNOBSERVABLE),
        }
        let frozen = match self.freezer.freeze_requested(target) {
            Ok(frozen) => frozen,
            // Zwischen Beobachtung und Lesen ersetzt oder verschwunden.
            Err(error) if error.is_target_replaced_or_gone() => {
                return self.clear(dir, target, CLEARED_VANISHED, RecordFate::Vanished);
            }
            Err(_) => return self.unresolved(target, UNRESOLVED_UNREADABLE),
        };
        if frozen {
            return self.keep_frozen(dir, entry);
        }
        let held = holds.iter().any(|hold| {
            hold.target.path == target.path && hold.target.kernel_id == target.kernel_id
        });
        if held {
            RecordFate::Kept {
                restored: false,
                completed: false,
            }
        } else {
            self.clear(dir, target, CLEARED_THAWED, RecordFate::Thawed)
        }
    }

    // Erst den Marker (nur eine reguläre Datei), dann den Datensatz. Lässt
    // sich der Marker nicht entfernen, bleibt auch der Datensatz (wie beim
    // Freigeben in `warden.rs`): ein signierter Thaw versucht es erneut.
    fn clear(
        &self,
        dir: &MarkerDir,
        target: &CgroupTarget,
        reason: &'static str,
        fate: RecordFate,
    ) -> RecordFate {
        if dir.remove(target.kernel_id).is_err() {
            return self.unresolved(target, UNRESOLVED_NOT_REMOVED);
        }
        // `forget` übernimmt die Änderung auch ohne Schreibzugriff im
        // Speicher; der Datensatz kommt dann beim nächsten Start wieder und
        // wird erneut (ohne Marker) abgeräumt.
        if self.containment.forget(target.kernel_id).is_err() {
            self.store_not_written(target);
        }
        self.audit.record(AuditEvent::ContainmentCleared {
            cgroup: target.path.clone(),
            kernel_id: target.kernel_id,
            reason,
        });
        fate
    }

    // Die cgroup ist eingefroren: die Eindämmung bleibt, ihr Marker muss da
    // sein (harw prüft nur ihn). Ein nicht abgeschlossener Datensatz wird
    // vorher als fremdes Bit abgeschlossen (Regel 5 der Moduldoku).
    fn keep_frozen(&self, dir: &MarkerDir, entry: &ContainmentEntry) -> RecordFate {
        let target = &entry.target;
        let completed = !entry.is_complete();
        if completed {
            // `activate` übernimmt auch ohne Schreibzugriff im Speicher.
            if self.containment.activate(target.kernel_id, true).is_err() {
                self.audit.record(AuditEvent::ContainmentUnresolved {
                    cgroup: Some(target.path.clone()),
                    kernel_id: target.kernel_id,
                    reason: UNRESOLVED_NOT_COMPLETED,
                });
            }
            self.audit.record(AuditEvent::ContainmentCompleted {
                cgroup: target.path.clone(),
                kernel_id: target.kernel_id,
            });
        }
        match dir.is_present(target.kernel_id) {
            Ok(true) => RecordFate::Kept {
                restored: false,
                completed,
            },
            Ok(false) => match dir.create(target.kernel_id) {
                // Zwischen Prüfen und Anlegen aufgetaucht.
                Ok(MarkerCreation::AlreadyPresent) => RecordFate::Kept {
                    restored: false,
                    completed,
                },
                Ok(MarkerCreation::Created | MarkerCreation::Replaced) => {
                    self.audit.record(AuditEvent::ContainmentRestored {
                        cgroup: target.path.clone(),
                        kernel_id: target.kernel_id,
                    });
                    RecordFate::Kept {
                        restored: true,
                        completed,
                    }
                }
                Err(_) => self.unresolved(target, UNRESOLVED_NOT_RESTORED),
            },
            // Ein Symlink oder Verzeichnis unter dem Namen: harw verweigert
            // `job.resume` dann ohnehin (fail closed); nichts anfassen.
            Err(_) => self.unresolved(target, UNRESOLVED_NOT_A_MARKER),
        }
    }

    // Der Speicher übernahm eine Änderung nur im Speicher; melden.
    fn store_not_written(&self, target: &CgroupTarget) {
        self.audit.record(AuditEvent::ContainmentUnresolved {
            cgroup: Some(target.path.clone()),
            kernel_id: target.kernel_id,
            reason: UNRESOLVED_STORE,
        });
    }

    // Lässt Datensatz und Marker unverändert und meldet es.
    fn unresolved(&self, target: &CgroupTarget, reason: &'static str) -> RecordFate {
        self.audit.record(AuditEvent::ContainmentUnresolved {
            cgroup: Some(target.path.clone()),
            kernel_id: target.kernel_id,
            reason,
        });
        RecordFate::Unresolved
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::PathBuf;

    use super::ContainmentReconciliation;
    use crate::audit::{AuditEvent, RecordingAuditSink};
    use crate::containment::{CONTAINMENT_FILE, ContainmentEntry, ContainmentMarkers};
    use crate::error::WardenError;
    use crate::executor::{CgroupV2Executor, RecordingExecutor};
    use crate::holds::HoldEntry;
    use crate::observe::CgroupFsObserver;
    use crate::policy::{ActionAllowlist, DenyList, WardenPolicy};
    use crate::state::WardenState;
    use crate::test_support::{TestError, TestResult, ctx, private_tempdir};
    use crate::warden::{Warden, WardenParts};
    use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget, KeyRing, ProofPolicy};
    use harw_types::{CgroupId, FindingId};
    use jiff::Timestamp;

    const NOW: i64 = 1_800_000_000;
    // Kernel-IDs ohne Fixture-cgroup: weit weg von jeder Inode eines Tempdirs.
    const ORPHAN_KID: u64 = u64::MAX - 1;
    const FOREIGN_KID: u64 = u64::MAX - 2;
    const ESCAPE_KID: u64 = u64::MAX - 3;
    // Zwei erfundene Boot-IDs im Format des Kernels.
    const BOOT_A: &str = "6c9a3f1e-2b47-4d8e-9a51-3e7f0c2d8b14";
    const BOOT_B: &str = "1f0e9d8c-7b6a-4c5d-8e3f-2a1b0c9d8e7f";

    fn at(offset_secs: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(NOW.saturating_add(offset_secs)).map_err(ctx("timestamp"))
    }

    fn kernel_id(raw: u64) -> TestResult<CgroupKernelId> {
        CgroupKernelId::new(raw).map_err(ctx("kernel id"))
    }

    fn target(path: &str, raw: u64) -> TestResult<CgroupTarget> {
        Ok(CgroupTarget::new(
            CgroupId::try_from_str(path).map_err(ctx("cgroup id"))?,
            kernel_id(raw)?,
            at(-3600)?,
        ))
    }

    // StateDirectory, Indexverzeichnis (`run/holds`, `0755` wie tmpfiles es
    // anlegt) und eine Fixture-cgroup-Wurzel; dazu die geteilte Aufzeichnung
    // des Audits.
    struct Setup {
        state: tempfile::TempDir,
        run: tempfile::TempDir,
        root: tempfile::TempDir,
        audit: RecordingAuditSink,
    }

    impl Setup {
        fn new() -> TestResult<Self> {
            let setup = Self {
                state: private_tempdir()?,
                run: tempfile::tempdir().map_err(ctx("run dir"))?,
                root: tempfile::tempdir().map_err(ctx("cgroup root"))?,
                audit: RecordingAuditSink::new(),
            };
            std::fs::create_dir(setup.holds()).map_err(ctx("holds dir"))?;
            std::fs::set_permissions(setup.holds(), std::fs::Permissions::from_mode(0o755))
                .map_err(ctx("chmod holds dir"))?;
            Ok(setup)
        }

        fn holds(&self) -> PathBuf {
            self.run.path().join("holds")
        }

        fn marker(&self, raw: u64) -> PathBuf {
            self.holds().join(raw.to_string())
        }

        fn cgroup_dir(&self, name: &str) -> PathBuf {
            self.root.path().join("harw.slice").join(name)
        }

        // Legt `harw.slice/<name>` mit `cgroup.freeze` = `bit` an und
        // liefert die Inode (= Kernel-ID).
        fn cgroup(&self, name: &str, bit: &[u8]) -> TestResult<u64> {
            let dir = self.cgroup_dir(name);
            std::fs::create_dir_all(&dir).map_err(ctx("create cgroup dir"))?;
            std::fs::write(dir.join("cgroup.freeze"), bit).map_err(ctx("write cgroup.freeze"))?;
            Ok(std::fs::metadata(&dir).map_err(ctx("metadata"))?.ino())
        }

        fn bit(&self, name: &str) -> TestResult<String> {
            std::fs::read_to_string(self.cgroup_dir(name).join("cgroup.freeze"))
                .map_err(ctx("read cgroup.freeze"))
        }

        // Ein Warden mit echter Beobachtung und echtem Lesen des Bits unter
        // der Fixture-Wurzel, über dem StateDirectory (wie nach einem
        // Neustart geöffnet).
        fn warden(&self) -> TestResult<Warden> {
            let state = WardenState::open(self.state.path(), at(0)?).map_err(ctx("state"))?;
            let calls = RecordingExecutor::new();
            Ok(Warden::new(WardenParts {
                freezer: Box::new(CgroupV2Executor::new(self.root.path())),
                thawer: Box::new(CgroupV2Executor::new(self.root.path())),
                isolator: Box::new(calls.clone()),
                killer: Box::new(calls),
                observer: Box::new(CgroupFsObserver::new(self.root.path())),
                audit: Box::new(self.audit.clone()),
                keys: KeyRing::new(),
                policy: WardenPolicy::new(
                    ProofPolicy::new(120, 5, vec!["harw.slice".to_owned()])
                        .map_err(ctx("proof policy"))?,
                    ActionAllowlist::freeze_and_thaw(),
                    DenyList::builtin(),
                ),
                state,
                markers: ContainmentMarkers::new(self.holds()),
            }))
        }

        // Stellt einen Datensatz (mit `freeze_was_set`) und auf Wunsch den
        // Marker, wie es ein Freeze vor einem Absturz getan hätte.
        fn contain(
            &self,
            warden: &Warden,
            target: &CgroupTarget,
            freeze_was_set: Option<bool>,
            marker: bool,
        ) -> TestResult {
            warden
                .state()
                .containment()
                .record(ContainmentEntry {
                    target: target.clone(),
                    freeze_was_set,
                    released: false,
                })
                .map_err(ctx("record"))?;
            if marker {
                ContainmentMarkers::new(self.holds())
                    .open()
                    .map_err(ctx("open index"))?
                    .create(target.kernel_id)
                    .map_err(ctx("create marker"))?;
            }
            Ok(())
        }

        fn records(&self, warden: &Warden) -> TestResult<Vec<CgroupKernelId>> {
            Ok(warden
                .state()
                .containment()
                .snapshot()
                .map_err(ctx("snapshot"))?
                .iter()
                .map(|entry| entry.target.kernel_id)
                .collect())
        }

        fn count(&self, predicate: impl Fn(&AuditEvent) -> bool) -> usize {
            self.audit
                .events()
                .iter()
                .filter(|&event| predicate(event))
                .count()
        }
    }

    fn path(name: &str) -> String {
        format!("harw.slice/{name}")
    }

    fn reconcile(warden: &Warden) -> TestResult<ContainmentReconciliation> {
        warden.reconcile_containments().map_err(ctx("reconcile"))
    }

    #[test]
    fn test_stale_marker_of_a_vanished_or_replaced_cgroup_is_removed() -> TestResult {
        let setup = Setup::new()?;
        let gone = setup.cgroup("gone", b"1\n")?;
        let replaced = setup.cgroup("replaced", b"1\n")?;
        let warden = setup.warden()?;
        setup.contain(&warden, &target(&path("gone"), gone)?, Some(false), true)?;
        setup.contain(
            &warden,
            &target(&path("replaced"), replaced)?,
            Some(false),
            true,
        )?;
        // Die eine cgroup ist weg; unter dem anderen Pfad liegt jetzt eine
        // neue (die alte bleibt geparkt, damit die Inode sicher eine andere
        // ist).
        std::fs::remove_dir_all(setup.cgroup_dir("gone")).map_err(ctx("remove cgroup"))?;
        std::fs::rename(setup.cgroup_dir("replaced"), setup.cgroup_dir("parked"))
            .map_err(ctx("park"))?;
        let successor = setup.cgroup("replaced", b"1\n")?;
        assert_ne!(successor, replaced);

        let report = reconcile(&warden)?;
        assert_eq!(report.vanished, 2);
        assert_eq!(report.kept, 0);
        assert_eq!(report.unresolved, 0);
        assert!(report.index_locked);
        assert!(!setup.marker(gone).exists());
        assert!(!setup.marker(replaced).exists());
        assert!(setup.records(&warden)?.is_empty());
        assert_eq!(
            setup.count(|event| matches!(event, AuditEvent::ContainmentCleared { .. })),
            2
        );
        // Die Nachfolgerin bleibt, wie sie war.
        assert_eq!(setup.bit("replaced")?, "1\n");
        // Ein zweiter Durchlauf findet nichts mehr.
        assert_eq!(
            reconcile(&warden)?,
            ContainmentReconciliation {
                index_locked: true,
                ..ContainmentReconciliation::default()
            }
        );
        Ok(())
    }

    #[test]
    fn test_marker_of_a_thawed_cgroup_without_hold_is_removed() -> TestResult {
        let setup = Setup::new()?;
        // Absturz zwischen `cgroup.freeze` = 0 und dem Entfernen des Markers.
        let released = setup.cgroup("released", b"0\n")?;
        // Absturz nach dem Marker, vor dem Lesen des Bits (nie eingefroren).
        let unfinished = setup.cgroup("unfinished", b"0\n")?;
        let warden = setup.warden()?;
        setup.contain(
            &warden,
            &target(&path("released"), released)?,
            Some(false),
            true,
        )?;
        setup.contain(
            &warden,
            &target(&path("unfinished"), unfinished)?,
            None,
            true,
        )?;

        let report = reconcile(&warden)?;
        assert_eq!(report.thawed, 2);
        assert_eq!(report.kept, 0);
        assert!(!setup.marker(released).exists());
        assert!(!setup.marker(unfinished).exists());
        assert!(setup.records(&warden)?.is_empty());
        // Der Abgleich schreibt nie `cgroup.freeze`.
        assert_eq!(setup.bit("released")?, "0\n");
        assert_eq!(setup.bit("unfinished")?, "0\n");
        Ok(())
    }

    #[test]
    fn test_frozen_containment_and_running_hold_are_kept() -> TestResult {
        let setup = Setup::new()?;
        let frozen = setup.cgroup("frozen", b"1\n")?;
        // Ein nicht abgeschlossener Datensatz bei gesetztem Bit bleibt auch
        // (Eindämmung geht vor); er wird abgeschlossen (eigener Test unten).
        let held = setup.cgroup("held", b"0\n")?;
        let warden = setup.warden()?;
        setup.contain(&warden, &target(&path("frozen"), frozen)?, Some(true), true)?;
        let held_target = target(&path("held"), held)?;
        setup.contain(&warden, &held_target, Some(false), true)?;
        warden
            .state()
            .holds()
            .arm(HoldEntry {
                target: held_target,
                thaw_at: at(60)?,
                finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding"))?,
                nonce: "00".repeat(16),
                attempts: 0,
            })
            .map_err(ctx("arm hold"))?;

        let report = reconcile(&warden)?;
        assert_eq!(report.kept, 2);
        assert_eq!(report.restored, 0);
        assert_eq!(report.completed, 0);
        assert_eq!(report.vanished, 0);
        assert_eq!(report.thawed, 0);
        assert_eq!(report.released, 0);
        assert_eq!(report.unresolved, 0);
        assert_eq!(report.orphaned, 0);
        for raw in [frozen, held] {
            assert!(setup.marker(raw).is_file());
        }
        assert_eq!(setup.records(&warden)?.len(), 2);
        assert_eq!(setup.bit("frozen")?, "1\n");
        assert_eq!(setup.bit("held")?, "0\n");
        assert!(setup.audit.events().is_empty());
        Ok(())
    }

    #[test]
    fn test_missing_marker_of_a_frozen_containment_is_restored() -> TestResult {
        let setup = Setup::new()?;
        let frozen = setup.cgroup("frozen", b"1\n")?;
        let warden = setup.warden()?;
        // Der Freeze lief ohne nutzbaren Index (Marker fehlt).
        setup.contain(
            &warden,
            &target(&path("frozen"), frozen)?,
            Some(false),
            false,
        )?;

        let report = reconcile(&warden)?;
        assert_eq!(report.kept, 1);
        assert_eq!(report.restored, 1);
        let marker = std::fs::symlink_metadata(setup.marker(frozen)).map_err(ctx("lstat"))?;
        assert!(marker.file_type().is_file());
        assert_eq!(
            marker.uid(),
            std::fs::symlink_metadata(setup.holds())
                .map_err(ctx("lstat index"))?
                .uid()
        );
        assert_eq!(
            setup.count(|event| matches!(event, AuditEvent::ContainmentRestored { .. })),
            1
        );
        assert_eq!(setup.bit("frozen")?, "1\n");
        Ok(())
    }

    #[test]
    fn test_incomplete_record_of_a_frozen_cgroup_is_completed_as_a_foreign_freeze() -> TestResult {
        let setup = Setup::new()?;
        // Absturz nach dem Marker bzw. nach dem Datensatz, jeweils vor dem
        // dauerhaften `freeze_was_set`: das gesetzte Bit stammt von harws
        // `job.suspend`, nicht vom Warden.
        let marked = setup.cgroup("marked", b"1\n")?;
        let unmarked = setup.cgroup("unmarked", b"1\n")?;
        let warden = setup.warden()?;
        setup.contain(&warden, &target(&path("marked"), marked)?, None, true)?;
        setup.contain(&warden, &target(&path("unmarked"), unmarked)?, None, false)?;

        let report = reconcile(&warden)?;
        assert_eq!(report.kept, 2);
        assert_eq!(report.completed, 2);
        assert_eq!(report.restored, 1);
        assert_eq!(report.unresolved, 0);
        // Beide sind abgeschlossen: das Bit gehört einem anderen, keine
        // Freigabe des Wardens hebt es auf; der Marker hält `job.resume` an.
        for (name, raw) in [("marked", marked), ("unmarked", unmarked)] {
            let entry = warden
                .state()
                .containment()
                .get(kernel_id(raw)?)
                .map_err(ctx("get"))?
                .ok_or(TestError::Missing("completed record"))?;
            assert_eq!(entry.freeze_was_set, Some(true));
            assert!(!entry.owns_freeze_bit());
            assert!(setup.marker(raw).is_file());
            assert_eq!(setup.bit(name)?, "1\n");
        }
        assert_eq!(
            setup.count(|event| matches!(event, AuditEvent::ContainmentCompleted { .. })),
            2
        );
        // Ein zweiter Durchlauf ändert nichts mehr.
        let again = reconcile(&warden)?;
        assert_eq!(again.kept, 2);
        assert_eq!(again.completed, 0);
        assert_eq!(again.restored, 0);
        Ok(())
    }

    #[test]
    fn test_orphan_marker_is_kept_and_reported() -> TestResult {
        let setup = Setup::new()?;
        let warden = setup.warden()?;
        ContainmentMarkers::new(setup.holds())
            .open()
            .map_err(ctx("open index"))?
            .create(kernel_id(ORPHAN_KID)?)
            .map_err(ctx("orphan marker"))?;
        let report = reconcile(&warden)?;
        assert_eq!(report.orphaned, 1);
        assert!(setup.marker(ORPHAN_KID).is_file());
        assert!(setup.audit.events().iter().any(|event| matches!(
            event,
            AuditEvent::ContainmentUnresolved { cgroup: None, kernel_id, .. }
                if kernel_id.get() == ORPHAN_KID
        )));
        Ok(())
    }

    #[test]
    fn test_symlinks_in_the_index_are_never_followed_or_removed() -> TestResult {
        let setup = Setup::new()?;
        let gone = setup.cgroup("gone", b"1\n")?;
        let frozen = setup.cgroup("frozen", b"1\n")?;
        let warden = setup.warden()?;
        setup.contain(&warden, &target(&path("gone"), gone)?, Some(false), false)?;
        setup.contain(
            &warden,
            &target(&path("frozen"), frozen)?,
            Some(false),
            false,
        )?;
        std::fs::remove_dir_all(setup.cgroup_dir("gone")).map_err(ctx("remove cgroup"))?;
        let outside = setup.run.path().join("outside");
        std::fs::write(&outside, b"foreign").map_err(ctx("write outside"))?;
        // Unter beiden Markernamen und unter einem Namen ohne Datensatz liegt
        // ein Symlink nach draußen.
        for raw in [gone, frozen, FOREIGN_KID] {
            std::os::unix::fs::symlink(&outside, setup.marker(raw))
                .map_err(ctx("symlink marker"))?;
        }

        let report = reconcile(&warden)?;
        assert_eq!(report.unresolved, 2);
        assert_eq!(report.vanished, 0);
        assert_eq!(report.kept, 0);
        assert_eq!(report.foreign, 3);
        assert_eq!(report.orphaned, 0);
        for raw in [gone, frozen, FOREIGN_KID] {
            assert!(
                std::fs::symlink_metadata(setup.marker(raw))
                    .map_err(ctx("lstat link"))?
                    .file_type()
                    .is_symlink()
            );
        }
        assert_eq!(
            std::fs::read(&outside).map_err(ctx("read outside"))?,
            b"foreign"
        );
        // Ohne entfernten Marker bleibt auch der Datensatz.
        assert_eq!(setup.records(&warden)?.len(), 2);
        assert_eq!(
            setup.count(|event| matches!(event, AuditEvent::ContainmentUnresolved { .. })),
            2
        );
        Ok(())
    }

    #[test]
    fn test_untrusted_or_missing_index_fails_the_reconciliation() -> TestResult {
        let setup = Setup::new()?;
        let gone = setup.cgroup("gone", b"1\n")?;
        let warden = setup.warden()?;
        setup.contain(&warden, &target(&path("gone"), gone)?, Some(false), true)?;
        std::fs::remove_dir_all(setup.cgroup_dir("gone")).map_err(ctx("remove cgroup"))?;

        // Ein Symlink an Stelle des Indexverzeichnisses: nichts wird gelesen
        // oder entfernt.
        let real = setup.run.path().join("real");
        std::fs::rename(setup.holds(), &real).map_err(ctx("move index"))?;
        std::os::unix::fs::symlink(&real, setup.holds()).map_err(ctx("symlink index"))?;
        assert!(matches!(
            warden.reconcile_containments(),
            Err(WardenError::UntrustedFile { .. })
        ));
        assert!(real.join(gone.to_string()).is_file());
        assert_eq!(setup.records(&warden)?.len(), 1);

        // Gruppen- oder weltschreibbar.
        std::fs::remove_file(setup.holds()).map_err(ctx("remove link"))?;
        std::fs::rename(&real, setup.holds()).map_err(ctx("restore index"))?;
        std::fs::set_permissions(setup.holds(), std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("chmod 0777"))?;
        assert!(matches!(
            warden.reconcile_containments(),
            Err(WardenError::UntrustedFile { .. })
        ));

        // Fehlt.
        std::fs::remove_dir_all(setup.holds()).map_err(ctx("remove index"))?;
        let Err(error) = warden.reconcile_containments() else {
            return Err(TestError::Unexpected("a missing index must fail".into()));
        };
        assert!(matches!(error, WardenError::Io(_)));
        assert_eq!(setup.records(&warden)?.len(), 1);
        assert!(setup.audit.events().is_empty());
        Ok(())
    }

    #[test]
    fn test_undecidable_records_stay_untouched() -> TestResult {
        let setup = Setup::new()?;
        let garbled = setup.cgroup("garbled", b"frozen")?;
        let warden = setup.warden()?;
        // Unlesbares Bit, und ein Pfad, den der Beobachter nicht auflöst.
        setup.contain(
            &warden,
            &target(&path("garbled"), garbled)?,
            Some(false),
            true,
        )?;
        setup.contain(
            &warden,
            &target("harw.slice/../escape", ESCAPE_KID)?,
            Some(false),
            true,
        )?;

        let report = reconcile(&warden)?;
        assert_eq!(report.unresolved, 2);
        assert_eq!(report.kept, 0);
        assert!(setup.marker(garbled).is_file());
        assert!(setup.marker(ESCAPE_KID).is_file());
        assert_eq!(setup.records(&warden)?.len(), 2);
        assert_eq!(setup.bit("garbled")?, "frozen");
        Ok(())
    }

    #[test]
    fn test_contended_index_lock_does_not_stop_the_reconciliation() -> TestResult {
        let setup = Setup::new()?;
        let released = setup.cgroup("released", b"0\n")?;
        let warden = setup.warden()?;
        setup.contain(
            &warden,
            &target(&path("released"), released)?,
            Some(false),
            true,
        )?;
        // Ein anderer Teilnehmer (jeder lokale Nutzer kann das) hält die Sperre.
        let other = std::fs::File::open(setup.holds()).map_err(ctx("open other"))?;
        other.lock().map_err(ctx("lock other"))?;

        let report = reconcile(&warden)?;
        assert!(!report.index_locked);
        assert_eq!(report.thawed, 1);
        assert!(!setup.marker(released).exists());
        other.unlock().map_err(ctx("unlock other"))?;
        Ok(())
    }

    #[test]
    fn test_unwritable_store_still_clears_the_marker_and_reports_it() -> TestResult {
        let setup = Setup::new()?;
        let gone = setup.cgroup("gone", b"1\n")?;
        let warden = setup.warden()?;
        setup.contain(&warden, &target(&path("gone"), gone)?, Some(false), true)?;
        std::fs::remove_dir_all(setup.cgroup_dir("gone")).map_err(ctx("remove cgroup"))?;
        // Jedes Schreiben des Speichers scheitert ab jetzt (`O_NOFOLLOW` auf
        // der Zwischendatei).
        std::os::unix::fs::symlink(
            setup.state.path().join("elsewhere"),
            setup
                .state
                .path()
                .join(format!("{}.tmp", crate::containment::CONTAINMENT_FILE)),
        )
        .map_err(ctx("block writes"))?;

        let report = reconcile(&warden)?;
        assert_eq!(report.vanished, 1);
        assert!(!setup.marker(gone).exists());
        // Im Speicher entfernt; gemeldet.
        assert!(setup.records(&warden)?.is_empty());
        assert_eq!(
            setup.count(|event| matches!(event, AuditEvent::ContainmentUnresolved { .. })),
            1
        );
        Ok(())
    }

    #[test]
    fn test_released_containment_loses_its_leftover_marker_whatever_the_bit() -> TestResult {
        let setup = Setup::new()?;
        // harws eigener Freeze steht noch; die Freigabe des Wardens lief
        // schon, nur ihr Marker blieb liegen.
        let frozen = setup.cgroup("frozen", b"1\n")?;
        let warden = setup.warden()?;
        let frozen_target = target(&path("frozen"), frozen)?;
        setup.contain(&warden, &frozen_target, Some(true), true)?;
        assert!(
            warden
                .state()
                .containment()
                .mark_released(frozen_target.kernel_id)
                .map_err(ctx("mark released"))?
        );

        let report = reconcile(&warden)?;
        assert_eq!(report.released, 1);
        assert_eq!(report.kept, 0);
        assert_eq!(report.restored, 0);
        assert!(!setup.marker(frozen).exists());
        assert!(setup.records(&warden)?.is_empty());
        assert_eq!(setup.bit("frozen")?, "1\n");
        assert_eq!(
            setup.count(|event| matches!(event, AuditEvent::ContainmentCleared { .. })),
            1
        );
        Ok(())
    }

    #[test]
    fn test_release_interrupted_after_the_marker_never_restores_it() -> TestResult {
        let setup = Setup::new()?;
        // harws Freeze steht noch. Die Freigabe des Wardens hat ihren
        // Datensatz dauerhaft als freigegeben gebucht und den Marker entfernt;
        // dann stürzte der Warden vor dem Entfernen des Datensatzes ab.
        let frozen = setup.cgroup("frozen", b"1\n")?;
        let frozen_target = target(&path("frozen"), frozen)?;
        {
            let crashed = setup.warden()?;
            setup.contain(&crashed, &frozen_target, Some(true), false)?;
            assert!(
                crashed
                    .state()
                    .containment()
                    .mark_released(frozen_target.kernel_id)
                    .map_err(ctx("mark released"))?
            );
        }

        let restarted = setup.warden()?;
        let report = reconcile(&restarted)?;
        assert_eq!(report.released, 1);
        assert_eq!(report.kept, 0);
        assert_eq!(report.restored, 0);
        // Die Freigabe ist bestätigt; kein Marker entsteht neu, `job.resume`
        // bleibt harw überlassen.
        assert!(!setup.marker(frozen).exists());
        assert!(setup.records(&restarted)?.is_empty());
        assert_eq!(setup.bit("frozen")?, "1\n");
        assert_eq!(
            setup.count(|event| matches!(event, AuditEvent::ContainmentRestored { .. })),
            0
        );
        assert_eq!(
            setup.count(|event| matches!(event, AuditEvent::ContainmentCleared { .. })),
            1
        );
        Ok(())
    }

    #[test]
    fn test_records_of_a_previous_boot_are_discarded_and_markers_left_alone() -> TestResult {
        // Ohne lesbare Boot-ID gibt es nichts zu vergleichen.
        let Some(current) = crate::containment::current_boot_id() else {
            return Ok(());
        };
        let previous = if current == BOOT_A { BOOT_B } else { BOOT_A };
        let setup = Setup::new()?;
        // Nach dem Neustart des Hosts trägt eine neue, eingefrorene cgroup
        // unter demselben Pfad zufällig dieselbe Kernel-ID wie der alte
        // Datensatz; ein Marker dieses Namens stammt aus diesem Boot.
        let reused = setup.cgroup("reused", b"1\n")?;
        let stale = serde_json::to_value(ContainmentEntry {
            target: target(&path("reused"), reused)?,
            freeze_was_set: Some(false),
            released: false,
        })
        .map_err(ctx("encode record"))?;
        let file = serde_json::json!({
            "version": 1,
            "boot_id": previous,
            "containments": [stale],
        });
        std::fs::write(
            setup.state.path().join(CONTAINMENT_FILE),
            serde_json::to_vec(&file).map_err(ctx("encode file"))?,
        )
        .map_err(ctx("write previous boot"))?;
        ContainmentMarkers::new(setup.holds())
            .open()
            .map_err(ctx("open index"))?
            .create(kernel_id(reused)?)
            .map_err(ctx("marker of this boot"))?;
        let warden = setup.warden()?;
        assert!(setup.records(&warden)?.is_empty());

        let report = reconcile(&warden)?;
        assert_eq!(report.vanished, 1);
        assert_eq!(report.kept, 0);
        assert_eq!(report.restored, 0);
        assert_eq!(report.orphaned, 1);
        // Weder Marker noch Bit der neuen cgroup wurden angefasst.
        assert!(setup.marker(reused).is_file());
        assert_eq!(setup.bit("reused")?, "1\n");
        assert!(setup.audit.events().iter().any(|event| matches!(
            event,
            AuditEvent::ContainmentCleared { reason, .. } if reason.contains("previous boot")
        )));
        // Die Datei gehört jetzt diesem Boot; ein Neustart findet nichts mehr.
        let written: serde_json::Value = serde_json::from_slice(
            &std::fs::read(setup.state.path().join(CONTAINMENT_FILE)).map_err(ctx("read file"))?,
        )
        .map_err(ctx("decode file"))?;
        assert_eq!(
            written.get("boot_id").and_then(serde_json::Value::as_str),
            Some(current.as_str())
        );
        let restarted = setup.warden()?;
        assert_eq!(reconcile(&restarted)?.vanished, 0);
        Ok(())
    }
}
