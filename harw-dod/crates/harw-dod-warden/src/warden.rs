//! Der Durchsetzer selbst: [`Warden`], [`WardenParts`], [`WardenOutcome`],
//! [`HoldSweep`] (PL-100 WP 3.1, W01 erweitert).
//!
//! # Verantwortungsbereich
//! [`Warden::handle`] ist die einzige Stelle dieser Crate, die eine
//! v2-Anfrage ([`WardenRequest`] mit [`harw_dod_warden_proto::SignedAuthorization`])
//! tatsächlich verarbeitet. Ein v1-Auftrag erreicht diese Funktion nie:
//! [`WardenRequest::decode`] lehnt ihn beim Dekodieren ausdrücklich ab
//! (`harw-warden`, `ipc.rs`). [`Warden::expire_holds`] ist der zweite und
//! letzte Einstiegspunkt: der Hold-Timer, der ausschließlich auftaut.
//!
//! # Prüfreihenfolge von [`Warden::handle`]
//! Der Peer-Check (`SO_PEERCRED` == Escalator-uid) liegt davor im Binary und
//! lässt fremde Peers gar nicht erst bis zum Dekodieren.
//!
//! 1. **Sperre**: alle Eingriffe und der Hold-Timer laufen nacheinander unter
//!    einem `Mutex` (deterministische Reihenfolge von Ledger und Holds);
//!    vergiftet → `Unavailable`.
//! 2. **Autorisierung** ([`WardenRequest::verify`]): Version, Key-ID im
//!    [`KeyRing`], MAC (konstante Zeit), Zeitfenster der Warden-eigenen
//!    [`harw_dod_warden_proto::ProofPolicy`], Aktionsbindung, stillgelegte
//!    Aktion, Stufe (Thaw ab `RuleTriggered`, LiftIsolation nur `Escalated`),
//!    `irreversible_ack` für Kill und Isolation (O4), Hold-Regeln,
//!    Freigebender, cgroup-Präfix und **zuletzt** die Nonce im persistenten
//!    [`crate::nonce_ledger::FileNonceLedger`].
//! 3. **Warden-Policy** ([`WardenPolicy::violation`]): Aktions-Allowlist und
//!    Deny-Liste → `PolicyDenied`.
//! 4. **Zielprüfung**: `first_seen <= issued_at`; die **jetzt** unter dem
//!    signierten Pfad beobachtete Kernel-ID ([`TargetObserver`]) gleicht der
//!    signierten → sonst `TargetChanged` (B21). Ausnahme: `LiftIsolation`
//!    einer verschwundenen cgroup ist zulässig (ihre Regeln hängen am Pfad;
//!    ohne diese Ausnahme blieben die Ketten für immer stehen), eine
//!    abweichende Kernel-ID bleibt auch dort abgelehnt. Läuft für dieselbe
//!    cgroup (Pfad und Kernel-ID) ein Hold, muss das signierte Tripel samt
//!    `first_seen` dem des Holds gleichen
//!    ([`VerifiedAuthorization::confirm_target`]): der Hold stammt aus einer
//!    früheren, ebenfalls signierten Autorisierung — die einzige vom Escalator
//!    unabhängige Aufzeichnung von `first_seen`, die der Warden hat.
//! 5. **Ausführung**: zuerst ein dauerhafter `attempt`-Eintrag im
//!    [`crate::action_ledger::ActionLedger`] (ohne ihn keine Wirkung), dann
//!    Hold-Verwaltung, dann der Ausführer, dann `executed`/`failed`. Freeze,
//!    Thaw und Kill bekommen das signierte Tripel; der produktive Ausführer
//!    prüft die Kernel-ID am geöffneten Verzeichnis und schreibt über
//!    denselben Deskriptor (`executor.rs`). Meldet er ein ersetztes oder
//!    verschwundenes Ziel, lautet die Antwort `TargetChanged` (Ledger:
//!    `failed`). Nach einer Isolation beobachtet der Warden das Ziel erneut:
//!    `nft` löst den Pfad selbst auf; liegt dort nicht mehr die signierte
//!    cgroup, wird die Isolation sofort aufgehoben (`TargetChanged`).
//!    Scheitert eine Isolation, entscheidet ebenfalls eine erneute
//!    Beobachtung, nicht die Fehlerart: `NotFound` kann beim
//!    Netfilter-Backend auch ein fehlendes `nft`-Programm sein
//!    (Fehlkonfiguration des Hosts → `ExecutionFailed`, nie
//!    `TargetChanged`). Das Aufheben einer Isolation meldet nie
//!    `TargetChanged`.
//!
//! # Holds (O3)
//! Ein Freeze mit `hold_secs` stellt **vor** dem Einfrieren einen
//! persistenten Hold (`thaw_at = now + hold_secs`); ein Freeze ohne Hold
//! (bis zur Aufhebung) hebt einen laufenden Hold desselben Pfads auf, ein
//! erfolgreicher Thaw oder Kill ebenso. Schlägt der Freeze fehl, wird der
//! vorherige Hold-Zustand wiederhergestellt. [`Warden::expire_holds`] taut
//! fällige Holds über den [`CgroupThawer`] auf — und über nichts sonst: der
//! Hold-Timer hebt nie eine Isolation auf und tötet nie. Vor dem Auftauen
//! wird das Ziel erneut beobachtet; ist es verschwunden oder ersetzt, wird
//! der Hold ohne Wirkung verworfen (`hold-dropped`). Ein fehlgeschlagenes
//! Auftauen wird mit wachsendem Abstand wiederholt ([`HOLD_RETRY_SECS`],
//! verdoppelt je Versuch, höchstens [`MAX_HOLD_RETRY_SECS`]); nach
//! [`MAX_HOLD_THAW_ATTEMPTS`] Fehlversuchen gibt der Timer auf
//! (`hold-abandoned` im Ledger, [`AuditEvent::HoldAbandoned`]): die cgroup
//! bleibt eingefroren, bis ein signierter Thaw sie freigibt — wie ein Freeze
//! „bis zur Aufhebung“ (O3, B13). So wächst weder das Ledger noch das
//! Journal ohne Grenze — auch dann nicht, wenn der Hold-Speicher nicht
//! schreibbar ist: neuer Abstand, Versuchszähler und das Entfernen eines
//! erledigten Holds gelten im Speicher trotzdem
//! ([`crate::holds::HoldStore::reschedule`],
//! [`crate::holds::HoldStore::forget`]); je Versuch entsteht höchstens eine
//! `Unavailable`-Meldung.
//!
//! Der Hold-Timer taut ohne eigene Autorisierung auf: die Haltedauer ist Teil
//! des vom Operator freigegebenen und MAC-gebundenen Freeze (`hold_secs`).
//!
//! # Immer eine Antwort
//! [`WardenOutcome::to_wire`] ist total: jede Variante, auch
//! [`WardenOutcome::ExecutionFailed`] und [`WardenOutcome::Unavailable`],
//! hat eine inhaltsfreie [`WardenResponse`]. Das Binary antwortet damit auf
//! jedem Pfad.
//!
//! # Audit vor jedem Fehlerpfad
//! Siehe `audit.rs`-Moduldoku. Ein [`AuditEvent::Attempting`] entsteht
//! erst nach dem dauerhaften `attempt`-Ledger-Eintrag und **vor** dem
//! Ausführer; jede Ablehnung hinterlässt [`AuditEvent::Denied`].
//!
//! # Nebenläufigkeit
//! [`Warden`] ist `Send + Sync` und wird als `Arc<Warden>` zwischen den
//! Verbindungs-Threads und dem Hold-Timer-Thread geteilt; die
//! Eingriffssperre serialisiert die Arbeit. Keine Systemuhr: `now` ist
//! immer Parameter (das Binary liest die Uhr).

use std::sync::Mutex;

use harw_dod_warden_proto::{
    Denial, KeyRing, VerifiedAuthorization, WardenAction, WardenActionAudit, WardenReply,
    WardenRequest, WardenResponse,
};
use jiff::{SignedDuration, Timestamp};

use crate::action_ledger::{LedgerEntry, LedgerEvent};
use crate::audit::{AuditEvent, AuditSink};
use crate::error::{WardenError, WardenResult};
use crate::executor::{CgroupFreezer, CgroupThawer, NetworkIsolator, ProcessTreeKiller};
use crate::hex;
use crate::holds::HoldEntry;
use crate::observe::TargetObserver;
use crate::policy::WardenPolicy;
use crate::state::WardenState;

/// Abstand, nach dem ein fehlgeschlagenes Auftauen eines Holds zum ersten Mal
/// wiederholt wird; jeder weitere Fehlversuch verdoppelt ihn.
pub const HOLD_RETRY_SECS: i64 = 30;

/// Größter Abstand zwischen zwei Auftau-Versuchen eines Holds.
pub const MAX_HOLD_RETRY_SECS: i64 = 3600;

/// Nach so vielen fehlgeschlagenen Auftau-Versuchen gibt der Hold-Timer auf
/// (`hold-abandoned`); mit den obigen Abständen rund 64 Minuten nach der
/// ersten Fälligkeit.
pub const MAX_HOLD_THAW_ATTEMPTS: u32 = 8;

/// Das Ergebnis einer verarbeiteten [`WardenRequest`].
///
/// # Description
/// Ein in-process-Typ dieser Crate; [`Self::to_wire`] bildet **jede**
/// Variante auf eine inhaltsfreie [`WardenResponse`] ab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WardenOutcome {
    /// Die Aktion wurde erfolgreich ausgeführt.
    Executed(WardenActionAudit),
    /// Autorisierung, Policy oder Zielprüfung hat die Anfrage abgelehnt.
    Denied(Denial),
    /// Alle Prüfungen waren erfolgreich, die Ausführung selbst ist
    /// fehlgeschlagen.
    ExecutionFailed,
    /// Der Warden konnte fail closed nicht arbeiten (Sperre vergiftet,
    /// Aktions-Ledger oder Hold-Speicher nicht schreibbar); nichts wurde
    /// ausgeführt.
    Unavailable,
}

impl WardenOutcome {
    /// Maps this outcome onto the content-free wire response (total).
    ///
    /// # Returns
    /// [`WardenResponse::Executed`] für [`Self::Executed`], sonst
    /// [`WardenResponse::Denied`] mit der Ablehnung bzw.
    /// [`Denial::ExecutionFailed`] / [`Denial::Unavailable`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden::WardenOutcome;
    /// use harw_dod_warden_proto::{Denial, WardenResponse};
    ///
    /// assert!(matches!(
    ///     WardenOutcome::ExecutionFailed.to_wire(),
    ///     WardenResponse::Denied { reason: Denial::ExecutionFailed }
    /// ));
    /// ```
    #[must_use]
    pub fn to_wire(&self) -> WardenResponse {
        match self {
            Self::Executed(audit) => WardenResponse::Executed {
                audit: audit.clone(),
            },
            Self::Denied(reason) => WardenResponse::Denied { reason: *reason },
            Self::ExecutionFailed => WardenResponse::Denied {
                reason: Denial::ExecutionFailed,
            },
            Self::Unavailable => WardenResponse::Denied {
                reason: Denial::Unavailable,
            },
        }
    }

    /// Wraps [`Self::to_wire`] in the versioned [`WardenReply`].
    #[must_use]
    pub fn to_reply(&self) -> WardenReply {
        WardenReply::new(self.to_wire())
    }
}

/// Ergebnis eines Durchlaufs von [`Warden::expire_holds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HoldSweep {
    /// Fällige Holds, deren cgroup aufgetaut wurde.
    pub thawed: usize,
    /// Fällige Holds, die ohne Wirkung verworfen wurden (Ziel geändert).
    pub dropped: usize,
    /// Fällige Holds, die später erneut versucht werden.
    pub retried: usize,
    /// Fällige Holds, deren Auftauen nach [`MAX_HOLD_THAW_ATTEMPTS`]
    /// Fehlversuchen aufgegeben wurde (cgroup bleibt eingefroren).
    pub abandoned: usize,
    /// Der Durchlauf konnte nicht arbeiten (Sperre oder Hold-Speicher).
    pub blocked: bool,
}

/// Die Bausteine eines [`Warden`].
///
/// # Description
/// Alle Ausführer, Beobachter und das Audit-Ziel als Trait-Objekte
/// (austauschbar zwischen Produktion und Tests), dazu Schlüsselring,
/// Warden-Policy und persistenter Zustand.
pub struct WardenParts {
    /// Ausführer für `FreezeCgroup`.
    pub freezer: Box<dyn CgroupFreezer + Send + Sync>,
    /// Ausführer für `ThawCgroup` und den Hold-Timer.
    pub thawer: Box<dyn CgroupThawer + Send + Sync>,
    /// Ausführer für `IsolateNetwork` und `LiftIsolation`.
    pub isolator: Box<dyn NetworkIsolator + Send + Sync>,
    /// Ausführer für `KillProcessTree`.
    pub killer: Box<dyn ProcessTreeKiller + Send + Sync>,
    /// Beobachter für die Zielprüfung.
    pub observer: Box<dyn TargetObserver + Send + Sync>,
    /// Audit-Ziel (Betreiber-Sicht).
    pub audit: Box<dyn AuditSink + Send + Sync>,
    /// Gültige Proof-Schlüssel.
    pub keys: KeyRing,
    /// Warden-eigene Policy.
    pub policy: WardenPolicy,
    /// Nonce-Ledger, Aktions-Ledger, Holds.
    pub state: WardenState,
}

/// Der Durchsetzer: prüft jede Autorisierung nach, führt zulässige Aktionen
/// über ihre Ausführungs-Traits aus, protokolliert jeden Pfad.
///
/// # Description
/// Siehe Moduldoku.
pub struct Warden {
    freezer: Box<dyn CgroupFreezer + Send + Sync>,
    thawer: Box<dyn CgroupThawer + Send + Sync>,
    isolator: Box<dyn NetworkIsolator + Send + Sync>,
    killer: Box<dyn ProcessTreeKiller + Send + Sync>,
    observer: Box<dyn TargetObserver + Send + Sync>,
    audit: Box<dyn AuditSink + Send + Sync>,
    keys: KeyRing,
    policy: WardenPolicy,
    state: WardenState,
    enforcement: Mutex<()>,
}

// Vorheriger Hold-Zustand eines Pfads, um ihn nach einem gescheiterten
// Freeze wiederherzustellen.
struct HoldUndo {
    previous: Option<HoldEntry>,
    changed: bool,
}

impl Warden {
    /// Builds a warden from its parts.
    ///
    /// # Examples
    /// Siehe die Tests in diesem Modul (`fixture`), die einen vollständigen
    /// Warden über einem Tempdir bauen.
    #[must_use]
    pub fn new(parts: WardenParts) -> Self {
        let WardenParts {
            freezer,
            thawer,
            isolator,
            killer,
            observer,
            audit,
            keys,
            policy,
            state,
        } = parts;
        Self {
            freezer,
            thawer,
            isolator,
            killer,
            observer,
            audit,
            keys,
            policy,
            state,
            enforcement: Mutex::new(()),
        }
    }

    /// Returns the warden policy (for startup logs).
    #[must_use]
    pub fn policy(&self) -> &WardenPolicy {
        &self.policy
    }

    /// Returns the key ring (key ids for startup logs; never key material).
    #[must_use]
    pub fn keys(&self) -> &KeyRing {
        &self.keys
    }

    /// Returns the persistent state.
    #[must_use]
    pub fn state(&self) -> &WardenState {
        &self.state
    }

    /// Verifies and, if everything passes, executes one v2 request.
    ///
    /// # Description
    /// Siehe Moduldoku für die Prüfreihenfolge. Jede Ablehnung wird über
    /// den [`AuditSink`] aufgezeichnet und als [`WardenOutcome::Denied`]
    /// zurückgegeben; es gibt keinen Pfad ohne Ergebnis.
    ///
    /// # Arguments
    /// - `request` (`&WardenRequest`): die dekodierte Anfrage.
    /// - `now` (`Timestamp`): Warden-Uhr (vom Binary injiziert).
    ///
    /// # Returns
    /// Das Ergebnis; [`WardenOutcome::to_reply`] ergibt die Antwort.
    ///
    /// # Concurrency
    /// Serialisiert über die Eingriffssperre.
    pub fn handle(&self, request: &WardenRequest, now: Timestamp) -> WardenOutcome {
        let Ok(_guard) = self.enforcement.lock() else {
            self.audit.record(AuditEvent::Unavailable {
                action: request.action.clone(),
                reason: "enforcement lock poisoned",
            });
            return WardenOutcome::Unavailable;
        };
        // Abgelaufene Nonces verwerfen; scheitert das, scheitert auch die
        // Nonce-Prüfung (fail closed).
        if self.state.nonces().prune(now).is_err() {
            return self.deny(&request.action, Denial::Unavailable);
        }
        let verified =
            match request.verify(&self.keys, now, self.policy.proof(), self.state.nonces()) {
                Ok(verified) => verified,
                Err(error) => return self.deny(&request.action, error.as_denial()),
            };
        if self.policy.violation(verified.action()).is_some() {
            return self.deny(verified.action(), Denial::PolicyDenied);
        }
        if let Err(denial) = self.confirm_live_target(&verified) {
            return self.deny(verified.action(), denial);
        }
        self.execute(&verified, now)
    }

    /// Thaws every hold due at `now` (and nothing else).
    ///
    /// # Description
    /// Siehe Moduldoku, Abschnitt „Holds“. Nur [`CgroupThawer::thaw`] wird
    /// aufgerufen — nie ein Isolator, nie ein Killer.
    ///
    /// # Arguments
    /// - `now` (`Timestamp`): Warden-Uhr.
    ///
    /// # Returns
    /// Die Zählung dieses Durchlaufs.
    ///
    /// # Concurrency
    /// Serialisiert über dieselbe Eingriffssperre wie [`Self::handle`].
    pub fn expire_holds(&self, now: Timestamp) -> HoldSweep {
        let mut sweep = HoldSweep::default();
        let Ok(_guard) = self.enforcement.lock() else {
            sweep.blocked = true;
            return sweep;
        };
        let Ok(due) = self.state.holds().due(now) else {
            sweep.blocked = true;
            return sweep;
        };
        for hold in due {
            match self.expire_one(&hold, now) {
                HoldFate::Thawed => sweep.thawed = sweep.thawed.saturating_add(1),
                HoldFate::Dropped => sweep.dropped = sweep.dropped.saturating_add(1),
                HoldFate::Retried => sweep.retried = sweep.retried.saturating_add(1),
                HoldFate::Abandoned => sweep.abandoned = sweep.abandoned.saturating_add(1),
            }
        }
        sweep
    }

    // Zeichnet eine Ablehnung auf und gibt sie zurück.
    fn deny(&self, action: &WardenAction, reason: Denial) -> WardenOutcome {
        self.audit.record(AuditEvent::Denied {
            action: action.clone(),
            reason,
        });
        WardenOutcome::Denied(reason)
    }

    // Zielprüfung unmittelbar vor dem Eingriff (B21); siehe Moduldoku, Schritt 4.
    fn confirm_live_target(&self, verified: &VerifiedAuthorization) -> Result<(), Denial> {
        let target = verified.target();
        if target.first_seen > verified.issued_at() {
            return Err(Denial::TargetChanged);
        }
        let lifting = matches!(verified.action(), WardenAction::LiftIsolation { .. });
        match self.observer.kernel_id(&target.path) {
            Ok(observed) if observed == target.kernel_id => {}
            Ok(_) => return Err(Denial::TargetChanged),
            // Die Isolation einer verschwundenen cgroup darf aufgehoben werden:
            // es gibt keine lebende cgroup, die das Aufheben treffen könnte.
            Err(error) if error.is_target_gone() && lifting => return Ok(()),
            Err(error) if error.is_target_gone() => return Err(Denial::TargetChanged),
            Err(_) => return Err(Denial::Unavailable),
        }
        self.confirm_against_hold(verified)
    }

    // Ein laufender Hold derselben cgroup (Pfad und Kernel-ID) bindet ein
    // Tripel aus einer früheren signierten Autorisierung: `first_seen` muss
    // gleich sein.
    fn confirm_against_hold(&self, verified: &VerifiedAuthorization) -> Result<(), Denial> {
        let target = verified.target();
        let Ok(holds) = self.state.holds().snapshot() else {
            return Err(Denial::Unavailable);
        };
        let held = holds.iter().find(|hold| {
            hold.target.path == target.path && hold.target.kernel_id == target.kernel_id
        });
        match held {
            Some(hold) => verified
                .confirm_target(&hold.target)
                .map_err(|_| Denial::TargetChanged),
            None => Ok(()),
        }
    }

    // Ledger → Hold → Ausführer → Ledger, mit Audit an jeder Stelle.
    fn execute(&self, verified: &VerifiedAuthorization, now: Timestamp) -> WardenOutcome {
        let action = verified.action();
        let attempt = LedgerEntry::for_authorization(LedgerEvent::Attempt, verified);
        if self.state.actions().append(now, &attempt).is_err() {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "action ledger not writable; nothing executed",
            });
            return WardenOutcome::Unavailable;
        }
        self.audit.record(AuditEvent::Attempting {
            action: action.clone(),
        });

        let undo = match self.prepare_hold(verified, now) {
            Ok(undo) => undo,
            Err(_) => {
                self.record_outcome(now, verified, LedgerEvent::Failed);
                self.audit.record(AuditEvent::Unavailable {
                    action: action.clone(),
                    reason: "hold store not writable; nothing executed",
                });
                return WardenOutcome::Unavailable;
            }
        };

        match self.dispatch(verified) {
            Ok(()) => {
                if !self.isolation_hit_the_signed_target(verified) {
                    self.record_outcome(now, verified, LedgerEvent::Failed);
                    return self.deny(action, Denial::TargetChanged);
                }
                self.record_outcome(now, verified, LedgerEvent::Executed);
                self.settle_hold_after_success(action);
                let audit = WardenActionAudit::for_action(action.clone());
                self.audit.record(AuditEvent::Executed {
                    audit: audit.clone(),
                });
                WardenOutcome::Executed(audit)
            }
            // Das Ziel war beim Eingriff nicht mehr die geprüfte cgroup; der
            // Ausführer hat nichts geschrieben. Beim Aufheben einer Isolation
            // ist ein „verschwundenes“ Ziel kein Grund (siehe Schritt 4).
            Err(error) if self.failure_was_the_target(verified, &error) => {
                self.record_outcome(now, verified, LedgerEvent::Failed);
                self.restore_hold(action, undo);
                self.deny(action, Denial::TargetChanged)
            }
            Err(_) => {
                self.record_outcome(now, verified, LedgerEvent::Failed);
                self.restore_hold(action, undo);
                self.audit.record(AuditEvent::ExecutionFailed {
                    action: action.clone(),
                });
                WardenOutcome::ExecutionFailed
            }
        }
    }

    // Ob ein Ausführungsfehler am Ziel lag (ersetzt oder verschwunden) statt
    // am Ausführer. Freeze, Thaw und Kill: der Ausführer prüft am geöffneten
    // Deskriptor und meldet es selbst. Isolation: erneut beobachten — die
    // Fehlerart des Netfilter-Backends ist dafür kein Beleg (`NotFound` auch
    // bei fehlendem `nft`). Aufheben einer Isolation: nie (Schritt 4).
    fn failure_was_the_target(
        &self,
        verified: &VerifiedAuthorization,
        error: &WardenError,
    ) -> bool {
        match verified.action() {
            WardenAction::IsolateNetwork { cgroup } => match self.observer.kernel_id(cgroup) {
                Ok(observed) => observed != verified.target().kernel_id,
                Err(observe_error) => observe_error.is_target_gone(),
            },
            WardenAction::LiftIsolation { .. } => false,
            _ => error.is_target_replaced_or_gone(),
        }
    }

    // Nach einer Isolation: liegt unter dem Pfad noch die signierte cgroup?
    // `nft` löst den Pfad beim Einfügen selbst auf. Wurde die cgroup
    // dazwischen ersetzt, kann die Regel eine andere cgroup getroffen haben:
    // dann sofort aufheben und `false` melden. Für jede andere Aktion `true`.
    fn isolation_hit_the_signed_target(&self, verified: &VerifiedAuthorization) -> bool {
        let WardenAction::IsolateNetwork { cgroup } = verified.action() else {
            return true;
        };
        let target = verified.target();
        if matches!(self.observer.kernel_id(cgroup), Ok(observed) if observed == target.kernel_id) {
            return true;
        }
        if self.isolator.release_isolation(cgroup).is_err() {
            self.audit.record(AuditEvent::Unavailable {
                action: verified.action().clone(),
                reason: "isolation of a replaced target could not be undone",
            });
        }
        false
    }

    // Schreibt den Ergebniseintrag; ein Fehler wird gemeldet, ändert aber
    // das (bereits eingetretene) Ergebnis nicht.
    fn record_outcome(&self, now: Timestamp, verified: &VerifiedAuthorization, event: LedgerEvent) {
        let entry = LedgerEntry::for_authorization(event, verified);
        if self.state.actions().append(now, &entry).is_err() {
            self.audit.record(AuditEvent::Unavailable {
                action: verified.action().clone(),
                reason: "action ledger not writable; outcome not recorded",
            });
        }
    }

    // Vor einem Freeze: Hold stellen (mit Dauer) bzw. aufheben (bis zur
    // Aufhebung). Andere Aktionen ändern hier nichts.
    fn prepare_hold(
        &self,
        verified: &VerifiedAuthorization,
        now: Timestamp,
    ) -> WardenResult<HoldUndo> {
        let WardenAction::FreezeCgroup { cgroup } = verified.action() else {
            return Ok(HoldUndo {
                previous: None,
                changed: false,
            });
        };
        let previous = self
            .state
            .holds()
            .snapshot()?
            .into_iter()
            .find(|hold| hold.target.path == *cgroup);
        match verified.hold_secs() {
            Some(secs) => {
                let thaw_at = now
                    .checked_add(SignedDuration::from_secs(i64::from(secs)))
                    .map_err(|_| WardenError::LedgerUnavailable {
                        ledger: "holds",
                        reason: "hold end is out of range",
                    })?;
                self.state.holds().arm(HoldEntry {
                    target: verified.target().clone(),
                    thaw_at,
                    finding_id: verified.finding_id().clone(),
                    nonce: hex::encode(verified.nonce()),
                    attempts: 0,
                })?;
                self.audit.record(AuditEvent::HoldArmed {
                    cgroup: cgroup.clone(),
                    thaw_at,
                });
            }
            None => {
                self.state.holds().cancel(cgroup)?;
            }
        }
        Ok(HoldUndo {
            previous,
            changed: true,
        })
    }

    // Nach einem gescheiterten Freeze: vorherigen Hold-Zustand herstellen.
    fn restore_hold(&self, action: &WardenAction, undo: HoldUndo) {
        if !undo.changed {
            return;
        }
        let restored = match undo.previous {
            Some(previous) => self.state.holds().arm(previous),
            None => self.state.holds().cancel(action.cgroup()).map(|_| ()),
        };
        if restored.is_err() {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "hold store not writable; previous hold not restored",
            });
        }
    }

    // Nach erfolgreichem Thaw oder Kill: ein laufender Hold ist erledigt.
    fn settle_hold_after_success(&self, action: &WardenAction) {
        let finished = matches!(
            action,
            WardenAction::ThawCgroup { .. } | WardenAction::KillProcessTree { .. }
        );
        if finished && self.state.holds().cancel(action.cgroup()).is_err() {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "hold store not writable; finished hold kept",
            });
        }
    }

    // Ruft die zur Aktion passende Ausführungs-Trait-Methode auf. Freeze,
    // Thaw und Kill bekommen das signierte Tripel (Prüfung am Deskriptor).
    fn dispatch(&self, verified: &VerifiedAuthorization) -> WardenResult<()> {
        let target = verified.target();
        match verified.action() {
            WardenAction::FreezeCgroup { .. } => self.freezer.freeze(target),
            // Auftauen hebt nur den Freeze auf, nie eine Netz-Isolation.
            WardenAction::ThawCgroup { .. } => self.thawer.thaw(target),
            WardenAction::IsolateNetwork { cgroup } => self.isolator.isolate(cgroup),
            // Aufheben der Isolation lässt einen Freeze unberührt.
            WardenAction::LiftIsolation { cgroup } => self.isolator.release_isolation(cgroup),
            WardenAction::KillProcessTree { .. } => self.killer.kill(target),
            // Stillgelegt (PL-100 WP 3.1); `verify` lehnt die Variante vorher
            // mit `RetiredAction` ab, der Ausführer verweigert sie trotzdem.
            #[allow(deprecated)]
            WardenAction::ReleaseCgroup { .. } => Err(WardenError::RetiredAction),
        }
    }

    // Ein fälliger Hold: Ziel prüfen, auftauen oder verwerfen.
    fn expire_one(&self, hold: &HoldEntry, now: Timestamp) -> HoldFate {
        let action = WardenAction::ThawCgroup {
            cgroup: hold.target.path.clone(),
        };
        match self.observer.kernel_id(&hold.target.path) {
            Ok(observed) if observed == hold.target.kernel_id => {}
            Ok(_) => return self.drop_hold(hold, &action, now),
            Err(error) if error.is_target_gone() => return self.drop_hold(hold, &action, now),
            Err(_) => return self.retry_hold(hold, &action, now),
        }
        if self
            .state
            .actions()
            .append(now, &LedgerEntry::for_hold(LedgerEvent::Attempt, hold))
            .is_err()
        {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "action ledger not writable; hold thaw postponed",
            });
            return self.retry_hold(hold, &action, now);
        }
        self.audit.record(AuditEvent::Attempting {
            action: action.clone(),
        });
        match self.thawer.thaw(&hold.target) {
            Ok(()) => {
                self.record_hold_outcome(hold, &action, now, LedgerEvent::Executed);
                self.forget_hold(
                    hold,
                    &action,
                    "hold store not writable; thawed hold removed in memory only",
                );
                self.audit.record(AuditEvent::Executed {
                    audit: WardenActionAudit::for_action(action),
                });
                HoldFate::Thawed
            }
            // Zwischen Beobachtung und Schreiben ersetzt oder verschwunden:
            // nichts aufgetaut, Hold verwerfen.
            Err(error) if error.is_target_replaced_or_gone() => {
                self.record_hold_outcome(hold, &action, now, LedgerEvent::Failed);
                self.drop_hold(hold, &action, now)
            }
            Err(_) => {
                self.record_hold_outcome(hold, &action, now, LedgerEvent::Failed);
                self.audit.record(AuditEvent::ExecutionFailed {
                    action: action.clone(),
                });
                self.retry_hold(hold, &action, now)
            }
        }
    }

    // Ziel verschwunden oder ersetzt: nichts auftauen, Hold verwerfen.
    fn drop_hold(&self, hold: &HoldEntry, action: &WardenAction, now: Timestamp) -> HoldFate {
        self.record_hold_outcome(hold, action, now, LedgerEvent::HoldDropped);
        self.audit.record(AuditEvent::HoldDropped {
            cgroup: hold.target.path.clone(),
        });
        self.forget_hold(
            hold,
            action,
            "hold store not writable; dropped hold removed in memory only",
        );
        HoldFate::Dropped
    }

    // Später erneut versuchen (wachsender Abstand) oder nach
    // `MAX_HOLD_THAW_ATTEMPTS` Fehlversuchen aufgeben.
    fn retry_hold(&self, hold: &HoldEntry, action: &WardenAction, now: Timestamp) -> HoldFate {
        let attempts = hold.attempts.saturating_add(1);
        if attempts >= MAX_HOLD_THAW_ATTEMPTS {
            return self.abandon_hold(hold, action, now, attempts);
        }
        let retry_at = now
            .checked_add(SignedDuration::from_secs(hold_retry_delay_secs(
                hold.attempts,
            )))
            .unwrap_or(now);
        // `reschedule` übernimmt Abstand und Zähler auch ohne Schreibzugriff
        // im Speicher (`holds.rs`): Obergrenze und Abstand greifen trotzdem,
        // diese Meldung kommt höchstens einmal je Versuch.
        if self
            .state
            .holds()
            .reschedule(&hold.target.path, retry_at)
            .is_err()
        {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "hold store not writable; retry scheduled in memory only",
            });
        }
        HoldFate::Retried
    }

    // Auftauen aufgegeben: Ledger-Eintrag, Audit, Hold entfernen. Die cgroup
    // bleibt eingefroren, bis ein signierter Thaw sie freigibt.
    fn abandon_hold(
        &self,
        hold: &HoldEntry,
        action: &WardenAction,
        now: Timestamp,
        attempts: u32,
    ) -> HoldFate {
        self.record_hold_outcome(hold, action, now, LedgerEvent::HoldAbandoned);
        self.audit.record(AuditEvent::HoldAbandoned {
            cgroup: hold.target.path.clone(),
            attempts,
        });
        self.forget_hold(
            hold,
            action,
            "hold store not writable; abandoned hold removed in memory only",
        );
        HoldFate::Abandoned
    }

    // Ein erledigter Hold des Timers: im Speicher immer entfernen (er wird
    // in diesem Prozess nicht wieder fällig), Schreibfehler melden.
    fn forget_hold(&self, hold: &HoldEntry, action: &WardenAction, reason: &'static str) {
        if self.state.holds().forget(&hold.target.path).is_err() {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason,
            });
        }
    }

    // Ergebniseintrag des Hold-Timers.
    fn record_hold_outcome(
        &self,
        hold: &HoldEntry,
        action: &WardenAction,
        now: Timestamp,
        event: LedgerEvent,
    ) {
        if self
            .state
            .actions()
            .append(now, &LedgerEntry::for_hold(event, hold))
            .is_err()
        {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "action ledger not writable; hold outcome not recorded",
            });
        }
    }
}

// Ausgang eines einzelnen fälligen Holds.
enum HoldFate {
    Thawed,
    Dropped,
    Retried,
    Abandoned,
}

/// Returns the delay before the next thaw attempt after `failed_attempts` failures.
///
/// # Description
/// `HOLD_RETRY_SECS · 2^failed_attempts`, gesättigt und auf
/// [`MAX_HOLD_RETRY_SECS`] begrenzt.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::warden::{HOLD_RETRY_SECS, MAX_HOLD_RETRY_SECS, hold_retry_delay_secs};
///
/// assert_eq!(hold_retry_delay_secs(0), HOLD_RETRY_SECS);
/// assert_eq!(hold_retry_delay_secs(1), 60);
/// assert_eq!(hold_retry_delay_secs(40), MAX_HOLD_RETRY_SECS);
/// ```
#[must_use]
pub fn hold_retry_delay_secs(failed_attempts: u32) -> i64 {
    HOLD_RETRY_SECS
        .saturating_mul(2i64.saturating_pow(failed_attempts))
        .min(MAX_HOLD_RETRY_SECS)
}

#[cfg(test)]
mod tests {
    use super::{
        HOLD_RETRY_SECS, HoldSweep, MAX_HOLD_THAW_ATTEMPTS, Warden, WardenOutcome, WardenParts,
    };
    use crate::action_ledger::{LedgerEvent, LedgerOrigin, read_verified};
    use crate::audit::{AuditEvent, RecordingAuditSink};
    use crate::error::{WardenError, WardenResult};
    use crate::executor::{
        CgroupFreezer, NetworkIsolator, ProcessTreeKiller, RecordedCall, RecordingExecutor,
    };
    use crate::holds::HOLDS_FILE;
    use crate::observe::StaticTargetObserver;
    use crate::policy::{ACCEPTED_ACTION_KINDS, ActionAllowlist, DenyList, WardenPolicy};
    use crate::state::WardenState;
    use crate::test_support::{TestError, TestResult, ctx, private_tempdir};
    use harw_dod_warden_proto::{
        AuthorizationBody, CgroupKernelId, CgroupTarget, Denial, EscalationStage, KeyId, KeyRing,
        ProofKey, ProofPolicy, SignedAuthorization, WardenAction, WardenRequest, WardenResponse,
    };
    use harw_types::{ApprovalActor, CgroupId, FindingId};
    use jiff::Timestamp;
    use serde_json::Value;

    const KEY_OLD: [u8; 32] = [0x5a; 32];
    const KEY_NEW: [u8; 32] = [0x6b; 32];
    const KERNEL_ID: u64 = 4242;
    const NOW: i64 = 1_800_000_000;
    const JOB: &str = "harw.slice/job-1";

    // -- Bausteine -------------------------------------------------------------

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("cgroup id"))
    }

    fn at(offset_secs: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(NOW.saturating_add(offset_secs)).map_err(ctx("timestamp"))
    }

    fn kernel_id(raw: u64) -> TestResult<CgroupKernelId> {
        CgroupKernelId::new(raw).map_err(ctx("kernel id"))
    }

    fn key_id(id: &str) -> TestResult<KeyId> {
        KeyId::new(id).map_err(ctx("key id"))
    }

    fn ring(entries: &[(&str, [u8; 32])]) -> TestResult<KeyRing> {
        let mut ring = KeyRing::new();
        for (id, bytes) in entries {
            ring.insert(key_id(id)?, ProofKey::from_bytes(*bytes))
                .map_err(ctx("unique key id"))?;
        }
        Ok(ring)
    }

    fn proof_policy(prefixes: &[&str]) -> TestResult<ProofPolicy> {
        ProofPolicy::new(
            120,
            5,
            prefixes.iter().map(|prefix| (*prefix).to_owned()).collect(),
        )
        .map_err(ctx("proof policy"))
    }

    fn all_actions_policy(prefixes: &[&str], deny: DenyList) -> TestResult<WardenPolicy> {
        Ok(WardenPolicy::new(
            proof_policy(prefixes)?,
            ActionAllowlist::new(ACCEPTED_ACTION_KINDS).map_err(ctx("allowlist"))?,
            deny,
        ))
    }

    // Ein Warden über einem privaten Tempdir mit geteilten Aufzeichnungen.
    struct Fixture {
        dir: tempfile::TempDir,
        calls: RecordingExecutor,
        observer: StaticTargetObserver,
        audit: RecordingAuditSink,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let fixture = Self {
                dir: private_tempdir()?,
                calls: RecordingExecutor::new(),
                observer: StaticTargetObserver::new(),
                audit: RecordingAuditSink::new(),
            };
            fixture.observer.set(&cgroup(JOB)?, kernel_id(KERNEL_ID)?);
            Ok(fixture)
        }

        // Alle Bausteine mit der geteilten Aufzeichnung; Tests ersetzen
        // einzelne Felder vor `Warden::new`.
        fn parts(&self, keys: KeyRing, policy: WardenPolicy) -> TestResult<WardenParts> {
            let state = WardenState::open(self.dir.path(), at(0)?).map_err(ctx("state opens"))?;
            Ok(WardenParts {
                freezer: Box::new(self.calls.clone()),
                thawer: Box::new(self.calls.clone()),
                isolator: Box::new(self.calls.clone()),
                killer: Box::new(self.calls.clone()),
                observer: Box::new(self.observer.clone()),
                audit: Box::new(self.audit.clone()),
                keys,
                policy,
                state,
            })
        }

        fn default_parts(&self) -> TestResult<WardenParts> {
            self.parts(
                ring(&[("k-old", KEY_OLD)])?,
                all_actions_policy(&["harw.slice"], DenyList::builtin())?,
            )
        }

        fn warden_with(
            &self,
            keys: KeyRing,
            policy: WardenPolicy,
            freezer: RecordingExecutor,
        ) -> TestResult<Warden> {
            let mut parts = self.parts(keys, policy)?;
            parts.freezer = Box::new(freezer);
            Ok(Warden::new(parts))
        }

        fn warden(&self) -> TestResult<Warden> {
            Ok(Warden::new(self.default_parts()?))
        }
    }

    // Ausführer, der ein ersetztes Ziel meldet (wie `CgroupV2Executor` bei
    // abweichender Inode am geöffneten Verzeichnis).
    struct ReplacedTarget;

    impl CgroupFreezer for ReplacedTarget {
        fn freeze(&self, _target: &CgroupTarget) -> WardenResult<()> {
            Err(WardenError::TargetChanged)
        }
    }

    impl ProcessTreeKiller for ReplacedTarget {
        fn kill(&self, _target: &CgroupTarget) -> WardenResult<()> {
            Err(WardenError::TargetChanged)
        }
    }

    // Isolator, der beim Isolieren die cgroup unter dem Pfad ersetzt (wie ein
    // Neustart zwischen Zielprüfung und `nft`-Auflösung) und sonst aufzeichnet.
    struct SwappingIsolator {
        calls: RecordingExecutor,
        observer: StaticTargetObserver,
        replacement: CgroupKernelId,
    }

    impl NetworkIsolator for SwappingIsolator {
        fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()> {
            self.observer.set(cgroup, self.replacement);
            self.calls.isolate(cgroup)
        }

        fn release_isolation(&self, cgroup: &CgroupId) -> WardenResult<()> {
            self.calls.release_isolation(cgroup)
        }
    }

    // Isolator, dessen Backend wie ein fehlendes `nft`-Programm scheitert
    // (`NotFound`, `NftCommandBackend::spawn_error`); ersetzt auf Wunsch
    // vorher die cgroup unter dem Pfad.
    struct MissingNftIsolator {
        observer: StaticTargetObserver,
        replacement: Option<CgroupKernelId>,
    }

    impl NetworkIsolator for MissingNftIsolator {
        fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()> {
            if let Some(replacement) = self.replacement {
                self.observer.set(cgroup, replacement);
            }
            Err(WardenError::Io(std::io::Error::from(
                std::io::ErrorKind::NotFound,
            )))
        }

        fn release_isolation(&self, _cgroup: &CgroupId) -> WardenResult<()> {
            Err(WardenError::Io(std::io::Error::from(
                std::io::ErrorKind::NotFound,
            )))
        }
    }

    // Gültiger Inhalt; `irreversible_ack` folgt der Aktion (O4).
    fn body(
        action: &WardenAction,
        stage: EscalationStage,
        hold_secs: Option<u32>,
        nonce: u8,
    ) -> TestResult<AuthorizationBody> {
        Ok(AuthorizationBody {
            finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding id"))?,
            approver: ApprovalActor::Operator {
                id: "operator-1".to_owned(),
            },
            action: action.clone(),
            stage,
            target: CgroupTarget::new(action.cgroup().clone(), kernel_id(KERNEL_ID)?, at(-3600)?),
            hold_secs,
            irreversible_ack: action.requires_irreversible_ack(),
            nonce: [nonce; 16],
            issued_at: at(0)?,
            expires_at: at(60)?,
        })
    }

    fn signed(
        key: [u8; 32],
        kid: &str,
        action: &WardenAction,
        body: AuthorizationBody,
    ) -> TestResult<WardenRequest> {
        Ok(WardenRequest::new(
            action.clone(),
            SignedAuthorization::sign(&ProofKey::from_bytes(key), &key_id(kid)?, body),
        ))
    }

    fn request(
        action: &WardenAction,
        stage: EscalationStage,
        hold_secs: Option<u32>,
        nonce: u8,
    ) -> TestResult<WardenRequest> {
        signed(
            KEY_OLD,
            "k-old",
            action,
            body(action, stage, hold_secs, nonce)?,
        )
    }

    fn freeze() -> TestResult<WardenAction> {
        Ok(WardenAction::FreezeCgroup {
            cgroup: cgroup(JOB)?,
        })
    }

    fn denied(outcome: &WardenOutcome) -> Option<Denial> {
        match outcome {
            WardenOutcome::Denied(reason) => Some(*reason),
            _ => None,
        }
    }

    // Ersetzt den Wert unter `path` (ohne Indexzugriff).
    fn set_json(root: &mut Value, path: &[&str], replacement: Value) -> TestResult {
        let (last, parents) = path.split_last().ok_or(TestError::Missing("json path"))?;
        let mut cursor = root;
        for key in parents {
            cursor = cursor
                .get_mut(*key)
                .ok_or(TestError::Missing("json parent"))?;
        }
        cursor
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert((*last).to_owned(), replacement);
        Ok(())
    }

    fn ledger_events(fixture: &Fixture) -> TestResult<Vec<LedgerEvent>> {
        Ok(read_verified(fixture.dir.path())
            .map_err(ctx("ledger verifies"))?
            .iter()
            .map(|record| record.entry.event)
            .collect())
    }

    // -- Ausführung und Ledger ---------------------------------------------------

    #[test]
    fn test_verified_freeze_is_executed_after_ledger_attempt() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let action = freeze()?;
        let outcome = warden.handle(
            &request(&action, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?,
        );
        let WardenOutcome::Executed(audit) = outcome else {
            return Err(TestError::Unexpected(format!(
                "expected Executed, got {outcome:?}"
            )));
        };
        assert_eq!(audit.audit_name, "warden.freeze_cgroup");
        assert_eq!(
            fixture.calls.calls(),
            vec![RecordedCall::Freeze(cgroup(JOB)?)]
        );
        assert_eq!(
            ledger_events(&fixture)?,
            [LedgerEvent::Attempt, LedgerEvent::Executed]
        );
        let events = fixture.audit.events();
        assert!(matches!(
            events.first(),
            Some(AuditEvent::Attempting { .. })
        ));
        assert!(matches!(events.get(1), Some(AuditEvent::Executed { .. })));
        Ok(())
    }

    #[test]
    fn test_thaw_is_admissible_from_rule_triggered_and_lift_isolation_is_not() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let thaw = WardenAction::ThawCgroup {
            cgroup: cgroup(JOB)?,
        };
        let lift = WardenAction::LiftIsolation {
            cgroup: cgroup(JOB)?,
        };
        let thawed = warden.handle(
            &request(&thaw, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?,
        );
        assert!(matches!(thawed, WardenOutcome::Executed(_)));
        let refused = warden.handle(
            &request(&lift, EscalationStage::RuleTriggered, None, 2)?,
            at(1)?,
        );
        assert_eq!(denied(&refused), Some(Denial::NotAdmissibleAtStage));
        let lifted = warden.handle(
            &request(&lift, EscalationStage::Escalated, None, 3)?,
            at(1)?,
        );
        assert!(matches!(lifted, WardenOutcome::Executed(_)));
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Thaw(cgroup(JOB)?),
                RecordedCall::LiftIsolation(cgroup(JOB)?)
            ]
        );
        Ok(())
    }

    // -- Replay über einen Neustart (B24) ------------------------------------------

    #[test]
    fn test_replay_is_refused_after_a_simulated_restart() -> TestResult {
        let fixture = Fixture::new()?;
        let action = freeze()?;
        let replayed = request(&action, EscalationStage::RuleTriggered, None, 7)?;
        {
            let warden = fixture.warden()?;
            assert!(matches!(
                warden.handle(&replayed, at(1)?),
                WardenOutcome::Executed(_)
            ));
        }
        // Neuer Prozess: Zustand wird aus dem StateDirectory neu geladen.
        let restarted = fixture.warden()?;
        assert_eq!(
            denied(&restarted.handle(&replayed, at(2)?)),
            Some(Denial::ProofMismatch)
        );
        assert_eq!(fixture.calls.calls().len(), 1);
        Ok(())
    }

    // -- Deny-Liste und Allowlist (B12) -------------------------------------------------

    #[test]
    fn test_deny_listed_targets_are_refused_without_touching_anything() -> TestResult {
        let fixture = Fixture::new()?;
        let session = cgroup("user.slice/user-1000.slice/session-3.scope")?;
        let protected = cgroup("harw.slice/protected-1.scope")?;
        fixture.observer.set(&session, kernel_id(KERNEL_ID)?);
        fixture.observer.set(&protected, kernel_id(KERNEL_ID)?);
        let deny = DenyList::with_extra(&["harw.slice/protected-*.scope/**".to_owned()])
            .map_err(ctx("deny list"))?;
        let warden = fixture.warden_with(
            ring(&[("k-old", KEY_OLD)])?,
            all_actions_policy(&["harw.slice", "user.slice"], deny)?,
            fixture.calls.clone(),
        )?;
        for (nonce, target) in [(1u8, session), (2u8, protected)] {
            let action = WardenAction::FreezeCgroup { cgroup: target };
            let outcome = warden.handle(
                &request(&action, EscalationStage::RuleTriggered, None, nonce)?,
                at(1)?,
            );
            assert_eq!(denied(&outcome), Some(Denial::PolicyDenied));
        }
        assert!(fixture.calls.calls().is_empty());
        assert!(ledger_events(&fixture)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_action_outside_the_allowlist_is_refused() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden_with(
            ring(&[("k-old", KEY_OLD)])?,
            WardenPolicy::new(
                proof_policy(&["harw.slice"])?,
                ActionAllowlist::freeze_and_thaw(),
                DenyList::builtin(),
            ),
            fixture.calls.clone(),
        )?;
        let kill = WardenAction::KillProcessTree {
            cgroup: cgroup(JOB)?,
        };
        let outcome = warden.handle(
            &request(&kill, EscalationStage::Escalated, None, 1)?,
            at(1)?,
        );
        assert_eq!(denied(&outcome), Some(Denial::PolicyDenied));
        assert!(fixture.calls.calls().is_empty());
        Ok(())
    }

    // -- irreversible_ack (O4) -------------------------------------------------------------

    #[test]
    fn test_kill_and_isolate_without_irreversible_ack_are_refused() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        for (nonce, action) in [
            (
                1u8,
                WardenAction::KillProcessTree {
                    cgroup: cgroup(JOB)?,
                },
            ),
            (
                2u8,
                WardenAction::IsolateNetwork {
                    cgroup: cgroup(JOB)?,
                },
            ),
        ] {
            let mut unacknowledged = body(&action, EscalationStage::Escalated, None, nonce)?;
            unacknowledged.irreversible_ack = false;
            let outcome =
                warden.handle(&signed(KEY_OLD, "k-old", &action, unacknowledged)?, at(1)?);
            assert_eq!(denied(&outcome), Some(Denial::IrreversibleAckRequired));
            // Dieselbe Nonce mit Bestätigung ist danach noch gültig: die
            // Ablehnung hat keine Nonce verbraucht.
            let acknowledged = warden.handle(
                &request(&action, EscalationStage::Escalated, None, nonce)?,
                at(1)?,
            );
            assert!(matches!(acknowledged, WardenOutcome::Executed(_)));
        }
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Kill(cgroup(JOB)?),
                RecordedCall::Isolate(cgroup(JOB)?)
            ]
        );
        Ok(())
    }

    // -- Zielprüfung (B21) --------------------------------------------------------------------

    #[test]
    fn test_kernel_id_mismatch_or_vanished_target_is_refused() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let action = freeze()?;
        fixture
            .observer
            .set(&cgroup(JOB)?, kernel_id(KERNEL_ID.saturating_add(1))?);
        let reused = warden.handle(
            &request(&action, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?,
        );
        assert_eq!(denied(&reused), Some(Denial::TargetChanged));
        fixture.observer.remove(&cgroup(JOB)?);
        let vanished = warden.handle(
            &request(&action, EscalationStage::RuleTriggered, None, 2)?,
            at(1)?,
        );
        assert_eq!(denied(&vanished), Some(Denial::TargetChanged));
        assert!(fixture.calls.calls().is_empty());
        Ok(())
    }

    #[test]
    fn test_first_seen_after_issue_is_refused() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let action = freeze()?;
        let mut implausible = body(&action, EscalationStage::RuleTriggered, None, 1)?;
        implausible.target.first_seen = at(10)?;
        let outcome = warden.handle(&signed(KEY_OLD, "k-old", &action, implausible)?, at(1)?);
        assert_eq!(denied(&outcome), Some(Denial::TargetChanged));
        Ok(())
    }

    // -- Holds (O3) ------------------------------------------------------------------------------

    #[test]
    fn test_hold_expiry_thaws_but_never_lifts_isolation() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let isolate = WardenAction::IsolateNetwork {
            cgroup: cgroup(JOB)?,
        };
        let freeze = freeze()?;
        assert!(matches!(
            warden.handle(
                &request(&isolate, EscalationStage::Escalated, None, 1)?,
                at(1)?
            ),
            WardenOutcome::Executed(_)
        ));
        assert!(matches!(
            warden.handle(
                &request(&freeze, EscalationStage::Escalated, Some(60), 2)?,
                at(1)?
            ),
            WardenOutcome::Executed(_)
        ));
        assert_eq!(warden.expire_holds(at(30)?), HoldSweep::default());
        let sweep = warden.expire_holds(at(61)?);
        assert_eq!(sweep.thawed, 1);
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Isolate(cgroup(JOB)?),
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Thaw(cgroup(JOB)?),
            ]
        );
        assert!(
            warden
                .state()
                .holds()
                .snapshot()
                .map_err(ctx("holds"))?
                .is_empty()
        );
        assert_eq!(warden.expire_holds(at(200)?), HoldSweep::default());
        Ok(())
    }

    #[test]
    fn test_hold_is_rearmed_after_restart_and_ledgered_as_timer() -> TestResult {
        let fixture = Fixture::new()?;
        let freeze = freeze()?;
        {
            let warden = fixture.warden()?;
            assert!(matches!(
                warden.handle(
                    &request(&freeze, EscalationStage::RuleTriggered, Some(60), 1)?,
                    at(1)?
                ),
                WardenOutcome::Executed(_)
            ));
        }
        let restarted = fixture.warden()?;
        assert_eq!(restarted.expire_holds(at(120)?).thawed, 1);
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Thaw(cgroup(JOB)?)
            ]
        );
        let records = read_verified(fixture.dir.path()).map_err(ctx("ledger"))?;
        let timer_events: Vec<LedgerEvent> = records
            .iter()
            .filter(|record| matches!(record.entry.origin, LedgerOrigin::HoldTimer { .. }))
            .map(|record| record.entry.event)
            .collect();
        assert_eq!(timer_events, [LedgerEvent::Attempt, LedgerEvent::Executed]);
        Ok(())
    }

    #[test]
    fn test_hold_is_dropped_when_the_target_changed() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let freeze = freeze()?;
        assert!(matches!(
            warden.handle(
                &request(&freeze, EscalationStage::RuleTriggered, Some(60), 1)?,
                at(1)?
            ),
            WardenOutcome::Executed(_)
        ));
        fixture.observer.set(&cgroup(JOB)?, kernel_id(9999)?);
        let sweep = warden.expire_holds(at(61)?);
        assert_eq!(sweep.dropped, 1);
        assert_eq!(
            fixture.calls.calls(),
            vec![RecordedCall::Freeze(cgroup(JOB)?)]
        );
        assert!(
            warden
                .state()
                .holds()
                .snapshot()
                .map_err(ctx("holds"))?
                .is_empty()
        );
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Executed,
                LedgerEvent::HoldDropped
            ]
        );
        Ok(())
    }

    #[test]
    fn test_freeze_until_released_and_thaw_cancel_a_running_hold() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let freeze = freeze()?;
        let thaw = WardenAction::ThawCgroup {
            cgroup: cgroup(JOB)?,
        };
        assert!(matches!(
            warden.handle(
                &request(&freeze, EscalationStage::RuleTriggered, Some(60), 1)?,
                at(1)?
            ),
            WardenOutcome::Executed(_)
        ));
        assert!(matches!(
            warden.handle(
                &request(&freeze, EscalationStage::RuleTriggered, None, 2)?,
                at(2)?
            ),
            WardenOutcome::Executed(_)
        ));
        assert!(
            warden
                .state()
                .holds()
                .snapshot()
                .map_err(ctx("holds"))?
                .is_empty()
        );
        assert!(matches!(
            warden.handle(
                &request(&freeze, EscalationStage::RuleTriggered, Some(60), 3)?,
                at(3)?
            ),
            WardenOutcome::Executed(_)
        ));
        assert!(matches!(
            warden.handle(
                &request(&thaw, EscalationStage::RuleTriggered, None, 4)?,
                at(4)?
            ),
            WardenOutcome::Executed(_)
        ));
        assert!(
            warden
                .state()
                .holds()
                .snapshot()
                .map_err(ctx("holds"))?
                .is_empty()
        );
        assert_eq!(warden.expire_holds(at(500)?), HoldSweep::default());
        Ok(())
    }

    // -- Jede Ablehnung und jeder Fehler hat eine Antwort ----------------------------------------

    #[test]
    fn test_execution_failure_replies_and_undoes_the_hold() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden_with(
            ring(&[("k-old", KEY_OLD)])?,
            all_actions_policy(&["harw.slice"], DenyList::builtin())?,
            fixture.calls.failing_view(),
        )?;
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
            at(1)?,
        );
        assert_eq!(outcome, WardenOutcome::ExecutionFailed);
        assert!(matches!(
            outcome.to_reply().outcome,
            WardenResponse::Denied {
                reason: Denial::ExecutionFailed
            }
        ));
        assert!(
            warden
                .state()
                .holds()
                .snapshot()
                .map_err(ctx("holds"))?
                .is_empty()
        );
        assert_eq!(
            ledger_events(&fixture)?,
            [LedgerEvent::Attempt, LedgerEvent::Failed]
        );
        assert!(matches!(
            fixture.audit.events().last(),
            Some(AuditEvent::ExecutionFailed { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_every_outcome_has_a_content_free_wire_reply() -> TestResult {
        let identifying = WardenAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/very-identifiable-cgroup")?,
        };
        let outcomes = [
            WardenOutcome::Executed(harw_dod_warden_proto::WardenActionAudit::for_action(
                identifying,
            )),
            WardenOutcome::Denied(Denial::PolicyDenied),
            WardenOutcome::ExecutionFailed,
            WardenOutcome::Unavailable,
        ];
        for outcome in &outcomes {
            let json = serde_json::to_string(&outcome.to_reply()).map_err(ctx("reply encodes"))?;
            assert!(json.contains("\"version\":2"));
            if !matches!(outcome, WardenOutcome::Executed(_)) {
                assert!(!json.contains("very-identifiable"));
            }
        }
        assert!(matches!(
            WardenOutcome::Unavailable.to_wire(),
            WardenResponse::Denied {
                reason: Denial::Unavailable
            }
        ));
        Ok(())
    }

    // -- MAC deckt jedes Feld (über die Leitung manipuliert) --------------------------------------

    #[test]
    fn test_tampering_any_signed_field_on_the_wire_is_refused() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let action = freeze()?;
        let original = request(&action, EscalationStage::RuleTriggered, Some(60), 9)?;
        let edits: [(&[&str], Value); 10] = [
            (&["finding_id"], Value::from("finding-2")),
            (&["approver", "id"], Value::from("operator-2")),
            (&["hold_secs"], Value::from(61)),
            (&["irreversible_ack"], Value::from(true)),
            (&["target", "kernel_id"], Value::from(4243)),
            (
                &["target", "first_seen"],
                Value::from("2026-01-01T00:00:00Z"),
            ),
            (&["expires_at"], Value::from("2027-01-15T08:00:30Z")),
            (&["nonce"], Value::from("0a".repeat(16))),
            (&["stage"], Value::from("escalated")),
            (&["key_id"], Value::from("k-other")),
        ];
        for (path, replacement) in edits {
            let mut json = serde_json::to_value(&original).map_err(ctx("encode"))?;
            let authorization = json
                .get_mut("authorization")
                .ok_or(TestError::Missing("authorization"))?;
            set_json(authorization, path, replacement)?;
            let bytes = serde_json::to_vec(&json).map_err(ctx("re-encode"))?;
            let tampered = WardenRequest::decode(&bytes).map_err(ctx("tampered shape decodes"))?;
            let outcome = warden.handle(&tampered, at(1)?);
            if denied(&outcome) != Some(Denial::ProofMismatch) {
                return Err(TestError::Unexpected(format!(
                    "tampered {path:?} must be refused, got {outcome:?}"
                )));
            }
        }
        assert!(fixture.calls.calls().is_empty());
        // Das unveränderte Original ist danach noch gültig (keine Nonce verbraucht).
        assert!(matches!(
            warden.handle(&original, at(1)?),
            WardenOutcome::Executed(_)
        ));
        Ok(())
    }

    // -- Schlüsselrotation ---------------------------------------------------------------------------

    #[test]
    fn test_old_key_id_is_accepted_until_removed() -> TestResult {
        let fixture = Fixture::new()?;
        let action = freeze()?;
        {
            let rotating = fixture.warden_with(
                ring(&[("k-old", KEY_OLD), ("k-new", KEY_NEW)])?,
                all_actions_policy(&["harw.slice"], DenyList::builtin())?,
                fixture.calls.clone(),
            )?;
            let old = rotating.handle(
                &request(&action, EscalationStage::RuleTriggered, None, 1)?,
                at(1)?,
            );
            assert!(matches!(old, WardenOutcome::Executed(_)));
            let new = signed(
                KEY_NEW,
                "k-new",
                &action,
                body(&action, EscalationStage::RuleTriggered, None, 2)?,
            )?;
            assert!(matches!(
                rotating.handle(&new, at(1)?),
                WardenOutcome::Executed(_)
            ));
        }
        let rotated = fixture.warden_with(
            ring(&[("k-new", KEY_NEW)])?,
            all_actions_policy(&["harw.slice"], DenyList::builtin())?,
            fixture.calls.clone(),
        )?;
        let old = rotated.handle(
            &request(&action, EscalationStage::RuleTriggered, None, 3)?,
            at(2)?,
        );
        assert_eq!(denied(&old), Some(Denial::ProofMismatch));
        let new = signed(
            KEY_NEW,
            "k-new",
            &action,
            body(&action, EscalationStage::RuleTriggered, None, 4)?,
        )?;
        assert!(matches!(
            rotated.handle(&new, at(2)?),
            WardenOutcome::Executed(_)
        ));
        Ok(())
    }

    // -- Stillgelegte Aktion ---------------------------------------------------------------------------

    #[test]
    #[allow(deprecated)]
    fn test_retired_release_cgroup_is_refused() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let release = WardenAction::ReleaseCgroup {
            cgroup: cgroup(JOB)?,
        };
        let outcome = warden.handle(
            &request(&release, EscalationStage::Escalated, None, 1)?,
            at(1)?,
        );
        assert_eq!(denied(&outcome), Some(Denial::Unsupported));
        assert!(fixture.calls.calls().is_empty());
        Ok(())
    }

    #[test]
    fn test_expired_authorization_is_refused_and_audited() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(61)?,
        );
        assert_eq!(denied(&outcome), Some(Denial::ProofMismatch));
        assert!(matches!(
            fixture.audit.events().first(),
            Some(AuditEvent::Denied {
                reason: Denial::ProofMismatch,
                ..
            })
        ));
        Ok(())
    }

    // -- Ziel zwischen Prüfung und Eingriff ersetzt (B21) --------------------------------------------

    #[test]
    fn test_executor_reporting_a_replaced_target_yields_target_changed() -> TestResult {
        let fixture = Fixture::new()?;
        let mut parts = fixture.default_parts()?;
        parts.freezer = Box::new(ReplacedTarget);
        parts.killer = Box::new(ReplacedTarget);
        let warden = Warden::new(parts);
        let kill = WardenAction::KillProcessTree {
            cgroup: cgroup(JOB)?,
        };
        let frozen = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
            at(1)?,
        );
        assert_eq!(denied(&frozen), Some(Denial::TargetChanged));
        let killed = warden.handle(
            &request(&kill, EscalationStage::Escalated, None, 2)?,
            at(1)?,
        );
        assert_eq!(denied(&killed), Some(Denial::TargetChanged));
        assert!(matches!(
            killed.to_wire(),
            WardenResponse::Denied {
                reason: Denial::TargetChanged
            }
        ));
        // Der vorab gestellte Hold wurde zurückgenommen.
        assert!(
            warden
                .state()
                .holds()
                .snapshot()
                .map_err(ctx("holds"))?
                .is_empty()
        );
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Failed,
                LedgerEvent::Attempt,
                LedgerEvent::Failed
            ]
        );
        assert!(matches!(
            fixture.audit.events().last(),
            Some(AuditEvent::Denied {
                reason: Denial::TargetChanged,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn test_isolation_that_hit_a_replaced_cgroup_is_undone_and_refused() -> TestResult {
        let fixture = Fixture::new()?;
        let mut parts = fixture.default_parts()?;
        parts.isolator = Box::new(SwappingIsolator {
            calls: fixture.calls.clone(),
            observer: fixture.observer.clone(),
            replacement: kernel_id(KERNEL_ID.saturating_add(7))?,
        });
        let warden = Warden::new(parts);
        let isolate = WardenAction::IsolateNetwork {
            cgroup: cgroup(JOB)?,
        };
        let outcome = warden.handle(
            &request(&isolate, EscalationStage::Escalated, None, 1)?,
            at(1)?,
        );
        assert_eq!(denied(&outcome), Some(Denial::TargetChanged));
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Isolate(cgroup(JOB)?),
                RecordedCall::LiftIsolation(cgroup(JOB)?)
            ]
        );
        assert_eq!(
            ledger_events(&fixture)?,
            [LedgerEvent::Attempt, LedgerEvent::Failed]
        );
        Ok(())
    }

    // -- LiftIsolation einer verschwundenen cgroup ---------------------------------------------------

    #[test]
    fn test_lift_isolation_of_a_vanished_cgroup_still_releases_its_rules() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let isolate = WardenAction::IsolateNetwork {
            cgroup: cgroup(JOB)?,
        };
        let lift = WardenAction::LiftIsolation {
            cgroup: cgroup(JOB)?,
        };
        assert!(matches!(
            warden.handle(
                &request(&isolate, EscalationStage::Escalated, None, 1)?,
                at(1)?
            ),
            WardenOutcome::Executed(_)
        ));
        fixture.observer.remove(&cgroup(JOB)?);
        let lifted = warden.handle(
            &request(&lift, EscalationStage::Escalated, None, 2)?,
            at(2)?,
        );
        assert!(matches!(lifted, WardenOutcome::Executed(_)));
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Isolate(cgroup(JOB)?),
                RecordedCall::LiftIsolation(cgroup(JOB)?)
            ]
        );
        // Andere Aktionen auf das verschwundene Ziel bleiben abgelehnt.
        let thaw = WardenAction::ThawCgroup {
            cgroup: cgroup(JOB)?,
        };
        let thawed = warden.handle(
            &request(&thaw, EscalationStage::Escalated, None, 3)?,
            at(2)?,
        );
        assert_eq!(denied(&thawed), Some(Denial::TargetChanged));
        Ok(())
    }

    #[test]
    fn test_lift_isolation_with_a_reused_kernel_id_is_still_refused() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        fixture
            .observer
            .set(&cgroup(JOB)?, kernel_id(KERNEL_ID.saturating_add(1))?);
        let lift = WardenAction::LiftIsolation {
            cgroup: cgroup(JOB)?,
        };
        let outcome = warden.handle(
            &request(&lift, EscalationStage::Escalated, None, 1)?,
            at(1)?,
        );
        assert_eq!(denied(&outcome), Some(Denial::TargetChanged));
        assert!(fixture.calls.calls().is_empty());
        Ok(())
    }

    // -- first_seen gegen einen laufenden Hold ------------------------------------------------------

    #[test]
    fn test_first_seen_must_match_a_running_hold_of_the_same_cgroup() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        assert!(matches!(
            warden.handle(
                &request(&freeze()?, EscalationStage::RuleTriggered, Some(600), 1)?,
                at(1)?
            ),
            WardenOutcome::Executed(_)
        ));
        let kill = WardenAction::KillProcessTree {
            cgroup: cgroup(JOB)?,
        };
        let mut other_incarnation = body(&kill, EscalationStage::Escalated, None, 2)?;
        other_incarnation.target.first_seen = at(-60)?;
        let outcome = warden.handle(&signed(KEY_OLD, "k-old", &kill, other_incarnation)?, at(2)?);
        assert_eq!(denied(&outcome), Some(Denial::TargetChanged));
        // Mit dem Tripel des Holds geht derselbe Kill durch.
        let consistent = warden.handle(
            &request(&kill, EscalationStage::Escalated, None, 3)?,
            at(2)?,
        );
        assert!(matches!(consistent, WardenOutcome::Executed(_)));
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Kill(cgroup(JOB)?)
            ]
        );
        Ok(())
    }

    // -- Hold-Timer: wachsender Abstand und Aufgabe -------------------------------------------------

    #[test]
    fn test_failing_hold_thaw_backs_off_and_is_abandoned_after_the_cap() -> TestResult {
        let fixture = Fixture::new()?;
        let mut parts = fixture.default_parts()?;
        parts.thawer = Box::new(fixture.calls.failing_view());
        let warden = Warden::new(parts);
        assert!(matches!(
            warden.handle(
                &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
                at(1)?
            ),
            WardenOutcome::Executed(_)
        ));
        let first = warden.expire_holds(at(61)?);
        assert_eq!(first.retried, 1);
        let holds = warden.state().holds().snapshot().map_err(ctx("holds"))?;
        let rescheduled = holds
            .first()
            .ok_or(TestError::Missing("rescheduled hold"))?;
        assert_eq!(rescheduled.attempts, 1);
        assert_eq!(
            rescheduled.thaw_at,
            at(61_i64.saturating_add(HOLD_RETRY_SECS))?
        );
        // Vor der neuen Fälligkeit passiert nichts.
        assert_eq!(warden.expire_holds(at(62)?), HoldSweep::default());

        let mut abandoned: usize = 0;
        let mut retried: usize = 1;
        for round in 1..MAX_HOLD_THAW_ATTEMPTS {
            let sweep = warden.expire_holds(at(100_000_i64.saturating_mul(i64::from(round)))?);
            retried = retried.saturating_add(sweep.retried);
            abandoned = abandoned.saturating_add(sweep.abandoned);
        }
        assert_eq!(abandoned, 1);
        assert_eq!(
            retried,
            usize::try_from(MAX_HOLD_THAW_ATTEMPTS.saturating_sub(1)).map_err(ctx("count"))?
        );
        assert!(
            warden
                .state()
                .holds()
                .snapshot()
                .map_err(ctx("holds"))?
                .is_empty()
        );
        assert_eq!(
            ledger_events(&fixture)?.last(),
            Some(&LedgerEvent::HoldAbandoned)
        );
        assert!(matches!(
            fixture.audit.events().last(),
            Some(AuditEvent::HoldAbandoned { attempts, .. }) if *attempts == MAX_HOLD_THAW_ATTEMPTS
        ));
        // Danach versucht der Timer nichts mehr.
        assert_eq!(warden.expire_holds(at(10_000_000)?), HoldSweep::default());
        Ok(())
    }

    #[test]
    fn test_unwritable_hold_store_still_backs_off_and_abandons_once() -> TestResult {
        let fixture = Fixture::new()?;
        let mut parts = fixture.default_parts()?;
        parts.thawer = Box::new(fixture.calls.failing_view());
        let warden = Warden::new(parts);
        assert!(matches!(
            warden.handle(
                &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
                at(1)?
            ),
            WardenOutcome::Executed(_)
        ));
        // Ab jetzt scheitert jedes Schreiben des Hold-Speichers
        // (`O_NOFOLLOW` auf der Zwischendatei), auch für root.
        std::os::unix::fs::symlink(
            fixture.dir.path().join("elsewhere"),
            fixture.dir.path().join(format!("{HOLDS_FILE}.tmp")),
        )
        .map_err(ctx("block the hold store"))?;

        assert_eq!(warden.expire_holds(at(61)?).retried, 1);
        // Der neue Abstand gilt im Speicher: eine Sekunde später passiert nichts.
        assert_eq!(warden.expire_holds(at(62)?), HoldSweep::default());
        let mut abandoned: usize = 0;
        for round in 1..=MAX_HOLD_THAW_ATTEMPTS {
            let sweep = warden.expire_holds(at(100_000_i64.saturating_mul(i64::from(round)))?);
            abandoned = abandoned.saturating_add(sweep.abandoned);
        }
        assert_eq!(abandoned, 1);
        // Der aufgegebene Hold steht noch in der Datei, wird in diesem
        // Prozess aber nie wieder fällig.
        assert_eq!(warden.expire_holds(at(10_000_000)?), HoldSweep::default());
        // Höchstens eine Meldung je Versuch (sieben verschobene Versuche und
        // die Aufgabe), keine Flut im Sekundentakt.
        let unavailable = fixture
            .audit
            .events()
            .iter()
            .filter(|event| matches!(event, AuditEvent::Unavailable { .. }))
            .count();
        assert_eq!(
            unavailable,
            usize::try_from(MAX_HOLD_THAW_ATTEMPTS).map_err(ctx("count"))?
        );
        Ok(())
    }

    // -- Netzaktionen: Fehler des Backends ist kein geändertes Ziel -----------------------------

    #[test]
    fn test_missing_nft_binary_is_an_execution_failure_not_a_changed_target() -> TestResult {
        let fixture = Fixture::new()?;
        let mut parts = fixture.default_parts()?;
        parts.isolator = Box::new(MissingNftIsolator {
            observer: fixture.observer.clone(),
            replacement: None,
        });
        let warden = Warden::new(parts);
        let isolate = WardenAction::IsolateNetwork {
            cgroup: cgroup(JOB)?,
        };
        let lift = WardenAction::LiftIsolation {
            cgroup: cgroup(JOB)?,
        };
        let isolated = warden.handle(
            &request(&isolate, EscalationStage::Escalated, None, 1)?,
            at(1)?,
        );
        assert_eq!(isolated, WardenOutcome::ExecutionFailed);
        let lifted = warden.handle(
            &request(&lift, EscalationStage::Escalated, None, 2)?,
            at(1)?,
        );
        assert_eq!(lifted, WardenOutcome::ExecutionFailed);
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Failed,
                LedgerEvent::Attempt,
                LedgerEvent::Failed
            ]
        );
        Ok(())
    }

    #[test]
    fn test_failed_isolation_of_a_replaced_cgroup_is_target_changed() -> TestResult {
        let fixture = Fixture::new()?;
        let mut parts = fixture.default_parts()?;
        parts.isolator = Box::new(MissingNftIsolator {
            observer: fixture.observer.clone(),
            replacement: Some(kernel_id(KERNEL_ID.saturating_add(9))?),
        });
        let warden = Warden::new(parts);
        let isolate = WardenAction::IsolateNetwork {
            cgroup: cgroup(JOB)?,
        };
        let outcome = warden.handle(
            &request(&isolate, EscalationStage::Escalated, None, 1)?,
            at(1)?,
        );
        assert_eq!(denied(&outcome), Some(Denial::TargetChanged));
        assert_eq!(
            ledger_events(&fixture)?,
            [LedgerEvent::Attempt, LedgerEvent::Failed]
        );
        Ok(())
    }

    #[test]
    fn test_hold_retry_delay_doubles_and_is_capped() {
        assert_eq!(super::hold_retry_delay_secs(0), HOLD_RETRY_SECS);
        assert_eq!(super::hold_retry_delay_secs(2), 120);
        assert_eq!(
            super::hold_retry_delay_secs(u32::MAX),
            super::MAX_HOLD_RETRY_SECS
        );
    }
}
