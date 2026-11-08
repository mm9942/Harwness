//! Der Durchsetzer selbst: [`Warden`], [`WardenParts`], [`WardenOutcome`],
//! [`HoldSweep`] (PL-100 WP 3.1, W01 erweitert).
//!
//! # Verantwortungsbereich
//! [`Warden::handle`] ist die einzige Stelle dieser Crate, die eine
//! v2-Anfrage ([`WardenRequest`] mit [`harw_dod_warden_proto::SignedAuthorization`])
//! tatsächlich verarbeitet. Ein v1-Auftrag erreicht diese Funktion nie:
//! [`WardenRequest::decode`] lehnt ihn beim Dekodieren ausdrücklich ab
//! (`harw-warden`, `ipc.rs`). [`Warden::expire_holds`] ist der zweite
//! Einstiegspunkt: der Hold-Timer, der ausschließlich auftaut.
//! [`Warden::reconcile_containments`] läuft einmal beim Start und gleicht nur
//! Eindämmungsmarker und -datensätze ab (`reconcile.rs`); er greift in keine
//! cgroup ein. Dazu räumt der Hold-Timer höchstens alle
//! [`CONTAINMENT_SWEEP_SECS`] Eindämmungen verschwundener cgroups ab.
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
//!    abweichende Kernel-ID bleibt auch dort abgelehnt. Zweite Ausnahme, der
//!    Operator-Pfad: ein `ThawCgroup` einer cgroup, die es nicht mehr gibt
//!    (verschwunden oder unter dem Pfad ersetzt), deren Eindämmung der Warden
//!    aber noch führt (Datensatz desselben Pfads oder ein verwaister Marker
//!    der Kernel-ID), räumt nur Marker und Datensatz ab, berührt kein
//!    `cgroup.freeze` und antwortet `Executed` (der Hinweis steht dauerhaft
//!    im Aktions-Ledger als
//!    [`crate::action_ledger::LedgerEvent::StaleContainmentCleared`] statt
//!    `executed` und in [`AuditEvent::ContainmentCleared`]; die Antwort
//!    selbst ist inhaltsfrei). Lässt sich der Marker nicht prüfen (Symlink, Verzeichnis,
//!    unbrauchbarer Index), bleibt es bei `TargetChanged` und
//!    [`AuditEvent::ContainmentUnresolved`]. Läuft für dieselbe
//!    cgroup (Pfad und Kernel-ID) ein Hold, muss das signierte Tripel samt
//!    `first_seen` dem des Holds gleichen
//!    ([`VerifiedAuthorization::confirm_target`]): der Hold stammt aus einer
//!    früheren, ebenfalls signierten Autorisierung — die einzige vom Escalator
//!    unabhängige Aufzeichnung von `first_seen`, die der Warden hat.
//! 5. **Ausführung**: zuerst ein dauerhafter `attempt`-Eintrag im
//!    [`crate::action_ledger::ActionLedger`] (ohne ihn keine Wirkung), dann
//!    (nur Freeze) der Eindämmungsdatensatz, dann Hold-Verwaltung, dann der
//!    Ausführer, dann `executed`/`failed`. Freeze,
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
//! der Hold ohne Wirkung verworfen (`hold-dropped`). Ebenso, wenn der Warden
//! für die cgroup keinen (nicht freigegebenen) Eindämmungsdatensatz desselben
//! Pfads aus diesem Boot führt (ohne bekannte Boot-ID gilt kein gespeicherter
//! Datensatz als aus diesem Boot, `containment.rs`): jeder Freeze stellt ihn
//! vor dem Hold, ohne ihn hat ein anderer das Bit gesetzt (etwa nach einem
//! Neustart des Hosts eine neue cgroup mit derselben Kernel-ID), und der
//! Timer taut nicht auf (fail closed, B13). Ein fehlgeschlagenes
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
//! `Unavailable`-Meldung. Ein so nur im Speicher entfernter Hold kommt nicht
//! zurück: solange der Hold-Speicher ungesichert ist, schreibt auch das
//! Aufheben eines Holds durch einen Freeze ohne Haltedauer die Datei neu
//! (scheitert das: `Unavailable`, nichts ausgeführt), und der Hold-Timer
//! schreibt den Stand nach, sobald es geht
//! ([`crate::holds::HoldStore::flush`]).
//!
//! Der Hold-Timer taut ohne eigene Autorisierung auf: die Haltedauer ist Teil
//! des vom Operator freigegebenen und MAC-gebundenen Freeze (`hold_secs`).
//!
//! # Eindämmungsmarker für harw (Owner-Review 4505015305)
//! `job.resume` darf nie einen Freeze des Wardens aufheben. Ablauf (Vertrag
//! und Begründung in `containment.rs`):
//! - **Freeze**: vor dem Hold zuerst der dauerhafte Datensatz im
//!   [`crate::containment::ContainmentStore`] (scheitert er: `Unavailable`,
//!   nichts ausgeführt; ist der Speicher voll, räumt zuerst ein Durchlauf
//!   Eindämmungen verschwundener cgroups ab, bleibt er voll: `Unavailable`
//!   mit eigenem Grund), dann ein Versuch `flock(LOCK_EX | LOCK_NB)` auf das
//!   Indexverzeichnis, dann der Marker, dann `cgroup.freeze` lesen und
//!   `freeze_was_set` im Datensatz buchen, dann einfrieren, dann die Sperre
//!   freigeben. Sperre, Index, Marker oder Lesen scheitern: der
//!   Freeze läuft trotzdem ([`AuditEvent::ContainmentDegraded`]; ein
//!   unlesbares Bit gilt als nicht gesetzt). Ein erneuter Freeze einer schon
//!   eingedämmten cgroup behält ein `freeze_was_set` = `false` (das Bit ist
//!   dann sein eigenes); bei `true` liest er das Bit neu: hat der frühere
//!   Eigentümer seinen Freeze inzwischen aufgehoben, schreibt der Warden es
//!   jetzt selbst und bucht `false`. Lässt sich `freeze_was_set` nicht
//!   dauerhaft buchen, friert der Warden nicht ein (`Unavailable`, nichts
//!   ausgeführt): nach einem Absturz hielte der Abgleich das eigene Bit sonst
//!   für fremd. Scheitert der Freeze, geht ein neu angelegter Marker wieder
//!   weg, und der vorherige Datensatz gilt — auch die vorherige Zuordnung
//!   einer schon aktiven Eindämmung, die dieser Freeze eben umgebucht hatte.
//! - **Freigabe** (`ThawCgroup`, Hold-Timer): nur bei `freeze_was_set` =
//!   `false` gehört das Bit dem Warden. Sonst (schon gesetzt oder nicht
//!   abgeschlossen) bleibt es stehen, der frühere Eigentümer hebt seinen
//!   Freeze selbst auf, und die Freigabe besteht nur aus ihrer Buchung: sie
//!   steht **zuerst** dauerhaft fest
//!   ([`crate::containment::ContainmentStore::record_release`]); lässt sie
//!   sich nicht schreiben, antwortet ein signierter Thaw `Unavailable`
//!   (nichts freigegeben), und der Hold-Timer versucht es später erneut.
//!   Gehört das Bit dem Warden, setzt er zuerst `cgroup.freeze` = 0 und bucht
//!   danach die Freigabe (ein Absturz dazwischen hinterlässt Bit `0`; der
//!   Abgleich beim Start räumt ab oder lässt den Hold die Freigabe
//!   wiederholen). Erst danach folgen `executed` im Aktions-Ledger, das
//!   Aufheben des Holds, der Marker und zuletzt der Datensatz. Den Marker
//!   eines Datensatzes entfernt der Warden nur nach einer dauerhaft
//!   gebuchten Freigabe, damit ein Absturz nie einen Marker neu entstehen
//!   lässt. Ist sie nur im Speicher gebucht oder lässt sich der Marker nicht
//!   entfernen, bleiben Marker (fail closed) und Datensatz, als freigegeben
//!   markiert, und der Durchlauf des Hold-Timers schreibt die Freigabe nach
//!   und entfernt den Marker erneut (wie der Abgleich beim Start).
//! - **Verschwundene cgroups**: der Hold-Timer räumt beim Verwerfen eines
//!   Holds die Eindämmung ab, ein Kill prüft danach, ob die cgroup noch
//!   existiert (meist ja: `cgroup.kill` lässt das Verzeichnis stehen, bis
//!   systemd es entfernt), der Durchlauf des Hold-Timers räumt Eindämmungen
//!   verschwundener oder ersetzter cgroups ab, sobald er sie bemerkt, und der
//!   Operator-Pfad (Schritt 4) räumt jeden Rest ab. Eine Kernel-ID, die ein
//!   Datensatz eines anderen Pfads belegt, wird nie angefasst.
//! - **Neustart**: [`Warden::reconcile_containments`] räumt vor der ersten
//!   Anfrage Eindämmungen verschwundener cgroups, schon freigegebener
//!   Eindämmungen, nicht mehr eingefrorener cgroups ohne Hold (Absturz
//!   zwischen Auftauen und Entfernen) und Datensätze früherer oder nicht
//!   nachweisbar desselben Boots ab,
//!   schließt nicht abgeschlossene Datensätze eingefrorener cgroups als
//!   fremdes Bit ab, legt den fehlenden Marker einer noch eingefrorenen
//!   cgroup neu an und lässt Marker ohne Datensatz stehen (fail closed).
//! - Isolation friert nicht ein und legt deshalb keinen Marker an.
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
    CgroupKernelId, CgroupTarget, Denial, KeyRing, VerifiedAuthorization, WardenAction,
    WardenActionAudit, WardenReply, WardenRequest, WardenResponse,
};
use jiff::{SignedDuration, Timestamp};

use crate::action_ledger::{LedgerEntry, LedgerEvent};
use crate::audit::{AuditEvent, AuditSink};
use crate::containment::{ContainmentEntry, ContainmentMarkers, MarkerCreation, MarkerDir};
use crate::error::{WardenError, WardenResult};
use crate::executor::{CgroupFreezer, CgroupThawer, NetworkIsolator, ProcessTreeKiller};
use crate::hex;
use crate::holds::HoldEntry;
use crate::observe::TargetObserver;
use crate::policy::WardenPolicy;
use crate::reconcile::{ContainmentReconciliation, Reconciler};
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

/// Mindestabstand zwischen zwei Durchläufen über die Eindämmungen im
/// Hold-Timer ([`Warden::expire_holds`]): Eindämmungen verschwundener oder
/// ersetzter cgroups und liegengebliebene Marker freigegebener Eindämmungen
/// werden abgeräumt (`reconcile.rs`).
pub const CONTAINMENT_SWEEP_SECS: i64 = 10;

// Name des Eindämmungsspeichers in `WardenError::LedgerUnavailable`.
const CONTAINMENT_LEDGER: &str = "containment";

// Grund, mit dem `contain_and_freeze` einen Freeze verweigert, dessen
// `freeze_was_set` sich nicht dauerhaft buchen ließ (`execute` antwortet
// dann `Unavailable`).
const FREEZE_ATTRIBUTION_NOT_DURABLE: &str = "freeze_was_set not durable";

// Grund, mit dem `release_freeze` die Freigabe eines fremden Bits verweigert,
// deren Buchung sich nicht dauerhaft schreiben ließ (`execute` antwortet
// dann `Unavailable`, der Hold-Timer versucht es später erneut).
const RELEASE_NOT_DURABLE: &str = "release not durable";

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
    /// Aktions-Ledger, Hold- oder Eindämmungsspeicher nicht schreibbar, oder
    /// der Eindämmungsspeicher ist mit Eindämmungen lebender cgroups voll);
    /// nichts wurde ausgeführt.
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
    /// Fällige Holds, die ohne Wirkung verworfen wurden (Ziel geändert,
    /// oder der Warden führt keine Eindämmung der cgroup aus diesem Boot).
    pub dropped: usize,
    /// Fällige Holds, die später erneut versucht werden.
    pub retried: usize,
    /// Fällige Holds, deren Auftauen nach [`MAX_HOLD_THAW_ATTEMPTS`]
    /// Fehlversuchen aufgegeben wurde (cgroup bleibt eingefroren).
    pub abandoned: usize,
    /// Eindämmungen, deren Marker und Datensatz der Durchlauf über die
    /// Eindämmungen abgeräumt hat (cgroup verschwunden oder ersetzt, oder
    /// Freigabe schon gelaufen).
    pub cleared: usize,
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
    /// Nonce-Ledger, Aktions-Ledger, Holds, Eindämmungen.
    pub state: WardenState,
    /// Indexverzeichnis der Eindämmungsmarker für harw (Produktion:
    /// [`crate::containment::DEFAULT_HOLDS_DIR`]).
    pub markers: ContainmentMarkers,
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
    markers: ContainmentMarkers,
    enforcement: Mutex<()>,
    // Zeitpunkt des letzten Durchlaufs über die Eindämmungen (Hold-Timer).
    last_containment_sweep: Mutex<Option<Timestamp>>,
}

// Vorheriger Hold-Zustand eines Pfads, um ihn nach einem gescheiterten
// Freeze wiederherzustellen.
struct HoldUndo {
    previous: Option<HoldEntry>,
    changed: bool,
}

// Eindämmungsdatensatz eines Freeze vor dem Eingriff (Datensatz vor Marker).
#[derive(Default)]
struct ContainmentUndo {
    // Kernel-ID und vorheriger Datensatz, wenn dieser Freeze einen neuen
    // Datensatz gestellt hat (nach einem gescheiterten Freeze zurückstellen).
    replaced: Option<(CgroupKernelId, Option<ContainmentEntry>)>,
    // `freeze_was_set` einer schon aktiven Eindämmung derselben cgroup:
    // `false` übernimmt ein erneuter Freeze (das Bit ist sein eigenes),
    // `true` prüft er neu (der frühere Eigentümer kann es aufgehoben haben).
    known: Option<bool>,
    // Ob dieselbe cgroup schon vorher aktiv eingedämmt war (dann bleibt ihr
    // Marker auch nach einem gescheiterten Freeze).
    was_active: bool,
}

// Was die Zielprüfung ergeben hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetPresence {
    // Unter dem signierten Pfad liegt die signierte cgroup.
    Live,
    // Verschwunden; zulässig nur für `LiftIsolation`.
    VanishedLift,
    // Verschwunden oder ersetzt; ein `ThawCgroup` räumt nur die
    // liegengebliebene Eindämmung ab (Operator-Pfad).
    StaleContainment,
}

// Was die Beobachtung unter dem signierten Pfad zeigt.
enum Observed {
    Live,
    Replaced,
    Gone,
}

// Warum ein Freeze keinen Eindämmungsdatensatz stellen konnte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainmentRefusal {
    // Speicher vergiftet oder nicht schreibbar.
    Unwritable,
    // Speicher voll mit Eindämmungen lebender cgroups.
    Full,
}

impl ContainmentRefusal {
    // Fester Grund für das Audit.
    fn reason(self) -> &'static str {
        match self {
            Self::Unwritable => "containment store not writable; nothing executed",
            Self::Full => {
                "containment store full of containments of live cgroups; nothing executed"
            }
        }
    }
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
            markers,
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
            markers,
            enforcement: Mutex::new(()),
            last_containment_sweep: Mutex::new(None),
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

    /// Returns the containment marker index (for startup logs and reconciliation).
    #[must_use]
    pub fn markers(&self) -> &ContainmentMarkers {
        &self.markers
    }

    /// Reconciles containment records, markers, holds and cgroups (once, at startup).
    ///
    /// # Description
    /// Siehe [`crate::reconcile`] für die Regeln: Eindämmungen verschwundener
    /// oder ersetzter cgroups und nicht mehr eingefrorener cgroups ohne Hold
    /// werden abgeräumt (Marker zuerst), eingefrorene bleiben und bekommen
    /// einen fehlenden Marker zurück, alles Unklare bleibt unverändert. Folgt
    /// nie einem Symlink, schreibt nie `cgroup.freeze` und nichts ins
    /// Aktions-Ledger; jede Änderung steht im Audit. Das Binary ruft die
    /// Methode nach Landlock und seccomp und vor dem Hold-Timer und der
    /// ersten Anfrage auf.
    ///
    /// # Returns
    /// Die Zählung des Abgleichs.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Eingriffssperre oder Speicher
    ///   vergiftet.
    /// - [`WardenError::UntrustedFile`]: das Indexverzeichnis ist ein
    ///   Symlink, kein Verzeichnis, gehört nicht dem effektiven Nutzer oder
    ///   ist für Gruppe oder andere schreibbar.
    /// - [`WardenError::Io`]: das Indexverzeichnis fehlt oder lässt sich
    ///   nicht öffnen oder lesen.
    ///
    /// # Concurrency
    /// Serialisiert über dieselbe Eingriffssperre wie [`Self::handle`].
    pub fn reconcile_containments(&self) -> WardenResult<ContainmentReconciliation> {
        let _guard = self
            .enforcement
            .lock()
            .map_err(|_| WardenError::StatePoisoned)?;
        self.reconciler().run()
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
        match self.confirm_live_target(&verified) {
            Ok(TargetPresence::Live | TargetPresence::VanishedLift) => self.execute(&verified, now),
            Ok(TargetPresence::StaleContainment) => self.clear_stale_containment(&verified, now),
            Err(denial) => self.deny(verified.action(), denial),
        }
    }

    /// Thaws every hold due at `now` (and nothing else), then sweeps stale containments.
    ///
    /// # Description
    /// Siehe Moduldoku, Abschnitt „Holds“. Nur [`CgroupThawer::thaw`] wird
    /// aufgerufen — nie ein Isolator, nie ein Killer. Danach, höchstens alle
    /// [`CONTAINMENT_SWEEP_SECS`] (und nach einem Rücksprung der Uhr sofort),
    /// der Durchlauf über die Eindämmungen (`reconcile.rs`): Marker und
    /// Datensatz verschwundener oder ersetzter cgroups und liegengebliebene
    /// Marker freigegebener Eindämmungen gehen; lebende Eindämmungen bleiben
    /// unberührt, `cgroup.freeze` wird dabei nie geschrieben.
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
        if self.containment_sweep_due(now) {
            // Ein nur im Speicher gebuchter Stand des Hold-Speichers wird
            // nachgeschrieben, sobald er wieder schreibbar ist (`holds.rs`);
            // gemeldet wurde der Schreibfehler schon beim ersten Mal, ein
            // erneuter bleibt still.
            let _ = self.state.holds().flush();
            sweep.cleared = self.reconciler().sweep();
        }
        sweep
    }

    // Die Bausteine des Abgleichs (`reconcile.rs`), aus dem Warden geliehen.
    fn reconciler(&self) -> Reconciler<'_> {
        Reconciler {
            markers: &self.markers,
            containment: self.state.containment(),
            holds: self.state.holds(),
            observer: &*self.observer,
            freezer: &*self.freezer,
            audit: &*self.audit,
        }
    }

    // Ob der Durchlauf über die Eindämmungen fällig ist (höchstens alle
    // `CONTAINMENT_SWEEP_SECS`; nach einem Rücksprung der Uhr sofort); merkt
    // sich `now` als letzten Durchlauf.
    fn containment_sweep_due(&self, now: Timestamp) -> bool {
        let Ok(mut last) = self.last_containment_sweep.lock() else {
            return false;
        };
        let due = match *last {
            None => true,
            Some(previous) => {
                let elapsed = now.as_second().saturating_sub(previous.as_second());
                !(0..CONTAINMENT_SWEEP_SECS).contains(&elapsed)
            }
        };
        if due {
            *last = Some(now);
        }
        due
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
    fn confirm_live_target(
        &self,
        verified: &VerifiedAuthorization,
    ) -> Result<TargetPresence, Denial> {
        let target = verified.target();
        if target.first_seen > verified.issued_at() {
            return Err(Denial::TargetChanged);
        }
        let observed = match self.observer.kernel_id(&target.path) {
            Ok(observed) if observed == target.kernel_id => Observed::Live,
            Ok(_) => Observed::Replaced,
            Err(error) if error.is_target_gone() => Observed::Gone,
            Err(_) => return Err(Denial::Unavailable),
        };
        match (observed, verified.action()) {
            (Observed::Live, _) => self
                .confirm_against_hold(verified)
                .map(|()| TargetPresence::Live),
            // Die Isolation einer verschwundenen cgroup darf aufgehoben werden:
            // es gibt keine lebende cgroup, die das Aufheben treffen könnte.
            (Observed::Gone, WardenAction::LiftIsolation { .. }) => {
                Ok(TargetPresence::VanishedLift)
            }
            // Operator-Pfad: die signierte cgroup gibt es nicht mehr (cgroup v2
            // benennt nicht um); ein Thaw räumt nur ihre Eindämmung ab.
            (Observed::Gone | Observed::Replaced, WardenAction::ThawCgroup { .. }) => {
                match self.holds_stale_containment(target) {
                    Ok(true) => self
                        .confirm_against_hold(verified)
                        .map(|()| TargetPresence::StaleContainment),
                    Ok(false) => Err(Denial::TargetChanged),
                    Err(()) => Err(Denial::Unavailable),
                }
            }
            _ => Err(Denial::TargetChanged),
        }
    }

    // Ob der Warden für das (nicht mehr existierende) Ziel noch eine
    // Eindämmung führt: ein Datensatz desselben Pfads oder, ohne Datensatz
    // der Kernel-ID, ein verwaister Marker. Ein Datensatz eines anderen Pfads
    // mit derselben Kernel-ID gehört einer anderen Eindämmung: `false`. Ohne
    // Indexverzeichnis gibt es keinen Marker. Lässt sich der Marker nicht
    // prüfen (Symlink oder Verzeichnis unter dem Namen, unbrauchbarer Index),
    // wird nichts abgeräumt: `false` (der Thaw wird abgelehnt) und
    // `ContainmentUnresolved`. `Err(())` nur bei vergiftetem Speicher.
    fn holds_stale_containment(&self, target: &CgroupTarget) -> Result<bool, ()> {
        match self.state.containment().get(target.kernel_id) {
            Ok(Some(entry)) => Ok(entry.target.path == target.path),
            Ok(None) => match self.markers.is_present(target.kernel_id) {
                Ok(present) => Ok(present),
                Err(WardenError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(false)
                }
                Err(_) => {
                    self.audit.record(AuditEvent::ContainmentUnresolved {
                        cgroup: Some(target.path.clone()),
                        kernel_id: target.kernel_id,
                        reason: "marker of a vanished cgroup not checkable (not a regular file or index unusable); thaw denied, entry left in place",
                    });
                    Ok(false)
                }
            },
            Err(_) => Err(()),
        }
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

    // Ledger → Eindämmungsdatensatz → Hold → Ausführer → Ledger, mit Audit
    // an jeder Stelle.
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

        // Datensatz vor Hold: ein Hold, den der Hold-Timer auftauen darf,
        // hat immer schon einen dauerhaften Eindämmungsdatensatz
        // (`expire_one` taut ohne ihn nicht auf).
        let containment = match self.prepare_containment(verified) {
            Ok(containment) => containment,
            Err(refusal) => {
                self.record_outcome(now, verified, LedgerEvent::Failed);
                self.audit.record(AuditEvent::Unavailable {
                    action: action.clone(),
                    reason: refusal.reason(),
                });
                return WardenOutcome::Unavailable;
            }
        };
        let undo = match self.prepare_hold(verified, now) {
            Ok(undo) => undo,
            Err(_) => {
                self.restore_containment(action, containment);
                self.record_outcome(now, verified, LedgerEvent::Failed);
                self.audit.record(AuditEvent::Unavailable {
                    action: action.clone(),
                    reason: "hold store not writable; nothing executed",
                });
                return WardenOutcome::Unavailable;
            }
        };

        match self.dispatch(verified, &containment) {
            // `freeze_was_set` ließ sich nicht dauerhaft buchen, oder die
            // Freigabe eines fremden Bits nicht: weder Freeze noch Thaw hat
            // etwas geschrieben (`contain_and_freeze`, `release_freeze`).
            Err(WardenError::LedgerUnavailable {
                ledger: CONTAINMENT_LEDGER,
                reason: FREEZE_ATTRIBUTION_NOT_DURABLE | RELEASE_NOT_DURABLE,
            }) => {
                self.record_outcome(now, verified, LedgerEvent::Failed);
                self.restore_hold(action, undo);
                self.restore_containment(action, containment);
                self.audit.record(AuditEvent::Unavailable {
                    action: action.clone(),
                    reason: ContainmentRefusal::Unwritable.reason(),
                });
                WardenOutcome::Unavailable
            }
            Ok(()) => {
                if !self.isolation_hit_the_signed_target(verified) {
                    self.record_outcome(now, verified, LedgerEvent::Failed);
                    return self.deny(action, Denial::TargetChanged);
                }
                self.record_outcome(now, verified, LedgerEvent::Executed);
                self.settle_hold_after_success(action);
                self.settle_containment_after_success(action, verified.target());
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
                self.restore_containment(action, containment);
                self.deny(action, Denial::TargetChanged)
            }
            Err(_) => {
                self.record_outcome(now, verified, LedgerEvent::Failed);
                self.restore_hold(action, undo);
                self.restore_containment(action, containment);
                self.audit.record(AuditEvent::ExecutionFailed {
                    action: action.clone(),
                });
                WardenOutcome::ExecutionFailed
            }
        }
    }

    // Operator-Pfad (Schritt 4): die signierte cgroup gibt es nicht mehr;
    // nur ihr Marker und ihr Datensatz gehen, `cgroup.freeze` bleibt
    // unberührt. Ledger und Audit wie bei jedem Eingriff.
    fn clear_stale_containment(
        &self,
        verified: &VerifiedAuthorization,
        now: Timestamp,
    ) -> WardenOutcome {
        let action = verified.action();
        let target = verified.target();
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
        if self.remove_marker(target.kernel_id).is_err() {
            self.record_outcome(now, verified, LedgerEvent::Failed);
            self.degraded(action, "stale containment marker not removed");
            self.audit.record(AuditEvent::ExecutionFailed {
                action: action.clone(),
            });
            return WardenOutcome::ExecutionFailed;
        }
        if self.state.containment().forget(target.kernel_id).is_err() {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "containment store not writable; cleared containment removed in memory only",
            });
        }
        // Die Antwort lautet `Executed`; das Ledger hält dauerhaft fest, dass
        // nur die Eindämmung abgeräumt und nichts aufgetaut wurde.
        self.record_outcome(now, verified, LedgerEvent::StaleContainmentCleared);
        self.audit.record(AuditEvent::ContainmentCleared {
            cgroup: target.path.clone(),
            kernel_id: target.kernel_id,
            reason: "signed thaw of a cgroup that no longer exists; cgroup.freeze untouched",
        });
        let audit = WardenActionAudit::for_action(action.clone());
        self.audit.record(AuditEvent::Executed {
            audit: audit.clone(),
        });
        WardenOutcome::Executed(audit)
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

    // Vor einem Freeze: den Eindämmungsdatensatz dauerhaft stellen, bevor es
    // einen Marker gibt. Eine schon aktive Eindämmung derselben cgroup bleibt
    // (mit ihrem `freeze_was_set`). Ist der Speicher voll, räumt zuerst ein
    // Durchlauf Eindämmungen verschwundener cgroups ab; bleibt er voll,
    // `Full`. Andere Aktionen ändern hier nichts.
    fn prepare_containment(
        &self,
        verified: &VerifiedAuthorization,
    ) -> Result<ContainmentUndo, ContainmentRefusal> {
        let WardenAction::FreezeCgroup { .. } = verified.action() else {
            return Ok(ContainmentUndo::default());
        };
        let target = verified.target();
        let store = self.state.containment();
        let previous = store
            .get(target.kernel_id)
            .map_err(|_| ContainmentRefusal::Unwritable)?;
        if let Some(entry) = previous
            .as_ref()
            .filter(|entry| entry.target.path == target.path && entry.is_active())
        {
            return Ok(ContainmentUndo {
                replaced: None,
                known: entry.freeze_was_set,
                was_active: true,
            });
        }
        // Neu, nur ein nicht abgeschlossener Datensatz (Abbruch vor dem Lesen
        // des Bits, also auch vor dem Freeze) oder eine schon freigegebene
        // Eindämmung: neu stellen, das Bit wird frisch gelesen.
        let has_room = |store: &crate::containment::ContainmentStore| {
            store
                .has_room_for(target.kernel_id)
                .map_err(|_| ContainmentRefusal::Unwritable)
        };
        if !has_room(store)? {
            self.reconciler().sweep();
            if !has_room(store)? {
                return Err(ContainmentRefusal::Full);
            }
        }
        store
            .record(ContainmentEntry::containing(target.clone()))
            .map_err(|_| ContainmentRefusal::Unwritable)?;
        Ok(ContainmentUndo {
            replaced: Some((target.kernel_id, previous)),
            known: None,
            was_active: false,
        })
    }

    // Nach einem gescheiterten Freeze: den vorherigen Datensatz herstellen
    // (der Marker ist dann schon weg, siehe `contain_and_freeze`).
    fn restore_containment(&self, action: &WardenAction, undo: ContainmentUndo) {
        let Some((kernel_id, previous)) = undo.replaced else {
            return;
        };
        if self
            .state
            .containment()
            .restore(kernel_id, previous)
            .is_err()
        {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "containment store not writable; previous containment restored in memory only",
            });
        }
    }

    // Nach erfolgreichem Thaw: Marker und Datensatz freigeben. Nach einem
    // Kill: nur, wenn die cgroup schon nicht mehr existiert.
    fn settle_containment_after_success(&self, action: &WardenAction, target: &CgroupTarget) {
        match action {
            WardenAction::ThawCgroup { .. } => {
                self.release_containment(action, target);
            }
            WardenAction::KillProcessTree { .. } => {
                let vanished = match self.observer.kernel_id(&target.path) {
                    Ok(observed) => observed != target.kernel_id,
                    Err(error) => error.is_target_gone(),
                };
                if vanished && self.release_containment(action, target) {
                    self.audit.record(AuditEvent::ContainmentCleared {
                        cgroup: target.path.clone(),
                        kernel_id: target.kernel_id,
                        reason: "cgroup no longer exists after kill",
                    });
                }
            }
            _ => {}
        }
    }

    // Räumt Marker und Datensatz von `target` ab (nach der Freigabe bzw. für
    // eine verschwundene cgroup). Den Marker eines Datensatzes erst, wenn
    // dessen Freigabe dauerhaft gebucht ist: ein fremdes Bit hat
    // `release_freeze` schon streng gebucht, ein eigenes nach dem Auftauen
    // im Speicher (hier ggf. nachgeschrieben), sonst bucht es diese Methode.
    // Stürzt der Warden danach ab, findet der Abgleich beim Start einen
    // freigegebenen Datensatz und legt keinen Marker neu an (`reconcile.rs`,
    // Regel 1), auch wenn das Bit noch einem anderen gehört und gesetzt ist.
    // Ist die Freigabe nur im Speicher gebucht oder lässt sich der Marker
    // nicht entfernen, bleiben Marker (fail closed) und Datensatz, als
    // freigegeben markiert: der Durchlauf des Hold-Timers schreibt die
    // Freigabe nach und versucht es erneut, ohne ein Bit anzufassen. Belegt
    // ein Datensatz eines anderen Pfads die Kernel-ID, bleibt beides. Gibt
    // zurück, ob ein Marker oder Datensatz abgeräumt wurde.
    fn release_containment(&self, action: &WardenAction, target: &CgroupTarget) -> bool {
        let store = self.state.containment();
        let entry = match store.get(target.kernel_id) {
            Ok(entry) => entry,
            Err(_) => {
                self.audit.record(AuditEvent::Unavailable {
                    action: action.clone(),
                    reason: "containment store unavailable; marker kept",
                });
                return false;
            }
        };
        if entry
            .as_ref()
            .is_some_and(|entry| entry.target.path != target.path)
        {
            return false;
        }
        if let Some(entry) = &entry {
            // `mark_released` und `flush` übernehmen im Speicher; `Ok` heißt
            // hier: die Freigabe steht in der Datei.
            let durable = if entry.released {
                store.flush().is_ok()
            } else {
                store.mark_released(target.kernel_id).is_ok()
            };
            if !durable {
                self.audit.record(AuditEvent::Unavailable {
                    action: action.clone(),
                    reason: "containment store not writable; release not durable, marker kept until the hold timer writes it",
                });
                return false;
            }
        }
        let removed = match self.remove_marker(target.kernel_id) {
            Ok(removed) => removed,
            Err(_) => {
                // Die Freigabe ist dauerhaft gebucht; nur der Marker bleibt.
                // Der Durchlauf des Hold-Timers und der Abgleich beim Start
                // entfernen ihn erneut.
                self.degraded(
                    action,
                    "containment marker not removed after the release; the hold timer retries, job.resume stays refused until then",
                );
                return false;
            }
        };
        if entry.is_some() && store.forget(target.kernel_id).is_err() {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "containment store not writable; released containment removed in memory only",
            });
        }
        removed || entry.is_some()
    }

    // Entfernt den Marker von `kernel_id` (nur eine reguläre Datei); ohne
    // Indexverzeichnis gibt es keinen Marker, der bleiben müsste.
    fn remove_marker(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        match self.markers.remove(kernel_id) {
            Err(WardenError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            other => other,
        }
    }

    // Ein Schritt des Marker-Protokolls ist nicht gelungen; der Eingriff läuft weiter.
    fn degraded(&self, action: &WardenAction, reason: &'static str) {
        self.audit.record(AuditEvent::ContainmentDegraded {
            action: action.clone(),
            reason,
        });
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
    fn dispatch(
        &self,
        verified: &VerifiedAuthorization,
        containment: &ContainmentUndo,
    ) -> WardenResult<()> {
        let target = verified.target();
        match verified.action() {
            WardenAction::FreezeCgroup { .. } => self.contain_and_freeze(verified, containment),
            // Auftauen hebt nur den Freeze auf, nie eine Netz-Isolation.
            WardenAction::ThawCgroup { .. } => self.release_freeze(target),
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

    // Eindämmen (Vertrag mit harw, `containment.rs`): Sperre versuchen,
    // Marker zuerst, Bit lesen, `freeze_was_set` buchen, einfrieren, Sperre
    // freigeben. Eindämmung geht vor: Sperre, Index, Marker oder Lesen dürfen
    // scheitern; das dauerhafte Buchen von `freeze_was_set` nicht (dann
    // `LedgerUnavailable`, nichts eingefroren). Scheitert der Freeze, geht ein
    // von ihm angelegter Marker wieder weg (außer die cgroup war schon vorher
    // aktiv eingedämmt); den Datensatz stellt `restore_containment` zurück.
    fn contain_and_freeze(
        &self,
        verified: &VerifiedAuthorization,
        containment: &ContainmentUndo,
    ) -> WardenResult<()> {
        let action = verified.action();
        let target = verified.target();
        let dir = match self.markers.open_locked() {
            Ok(dir) => {
                if !dir.is_locked() {
                    self.degraded(
                        action,
                        "containment index lock not obtained; containing without it",
                    );
                }
                Some(dir)
            }
            Err(_) => {
                self.degraded(
                    action,
                    "containment index unusable; freezing without a marker",
                );
                None
            }
        };
        let created = match dir.as_ref().map(|dir| dir.create(target.kernel_id)) {
            Some(Ok(creation)) => creation != MarkerCreation::AlreadyPresent,
            Some(Err(_)) => {
                self.degraded(
                    action,
                    "containment marker not created; freezing without it",
                );
                false
            }
            None => false,
        };
        let remove_on_failure = created && !containment.was_active;
        let freeze_was_set = match containment.known {
            // Das Bit ist seit der ersten Eindämmung das eigene.
            Some(false) => false,
            // Neu, oder das Bit gehörte beim ersten Mal einem anderen: neu
            // lesen (hat er seinen Freeze inzwischen aufgehoben, schreibt der
            // Warden das Bit jetzt selbst).
            known => match self.freezer.freeze_requested(target) {
                Ok(was_set) => was_set,
                // Das Ziel ist nicht mehr die geprüfte cgroup: nicht einfrieren.
                Err(error) if error.is_target_replaced_or_gone() => {
                    self.undo_marker(action, dir.as_ref(), target, remove_on_failure);
                    return Err(error);
                }
                Err(_) if known == Some(true) => {
                    self.degraded(
                        action,
                        "cgroup.freeze unreadable; the earlier owner of the freeze bit is kept",
                    );
                    true
                }
                Err(_) => {
                    self.degraded(
                        action,
                        "cgroup.freeze unreadable; recorded as not set before the freeze",
                    );
                    false
                }
            },
        };
        // `freeze_was_set` steht dauerhaft fest, bevor `cgroup.freeze` = 1
        // geschrieben wird (nach einem Absturz weiß der Abgleich, wem das Bit
        // gehörte). Lässt es sich nicht dauerhaft buchen, friert der Warden
        // nicht ein: sonst hielte der Abgleich nach einem Absturz das eigene
        // Bit für fremd, und eine spätere Freigabe meldete `Executed`, ohne
        // aufzutauen. `execute` antwortet dann `Unavailable`.
        if containment.known != Some(freeze_was_set)
            && self
                .state
                .containment()
                .activate(target.kernel_id, freeze_was_set)
                .is_err()
        {
            self.restore_attribution(action, target, containment);
            self.undo_marker(action, dir.as_ref(), target, remove_on_failure);
            return Err(WardenError::LedgerUnavailable {
                ledger: CONTAINMENT_LEDGER,
                reason: FREEZE_ATTRIBUTION_NOT_DURABLE,
            });
        }
        if let Err(error) = self.freezer.freeze(target) {
            // Eine schon aktive Eindämmung, deren Zuordnung eben dauerhaft
            // umgebucht wurde (fremdes Bit, das inzwischen `0` war), bekommt
            // ihre frühere zurück: der Warden hat das Bit nicht geschrieben,
            // und eine spätere Freigabe darf ein dann wieder von harw
            // gesetztes Bit nicht als eigenes aufheben. Den Datensatz einer
            // neuen Eindämmung stellt `restore_containment` zurück.
            if containment
                .known
                .is_some_and(|known| known != freeze_was_set)
            {
                self.restore_attribution(action, target, containment);
            }
            self.undo_marker(action, dir.as_ref(), target, remove_on_failure);
            return Err(error);
        }
        // Erst jetzt, nach `cgroup.freeze` = 1, gibt das Schließen die Sperre frei.
        drop(dir);
        Ok(())
    }

    // Nach einem nicht dauerhaft gebuchten `freeze_was_set` (`activate` hat
    // den Speicher schon geändert) oder einem gescheiterten Freeze nach einer
    // dauerhaften Umbuchung: eine schon aktive Eindämmung bekommt ihre
    // frühere Zuordnung zurück, im Speicher und (soweit schreibbar) in der
    // Datei; den Datensatz einer neuen stellt `restore_containment` zurück.
    fn restore_attribution(
        &self,
        action: &WardenAction,
        target: &CgroupTarget,
        containment: &ContainmentUndo,
    ) {
        let Some(previous) = containment.known else {
            return;
        };
        if self
            .state
            .containment()
            .activate(target.kernel_id, previous)
            .is_err()
        {
            self.audit.record(AuditEvent::Unavailable {
                action: action.clone(),
                reason: "containment store not writable; previous containment restored in memory only",
            });
        }
    }

    // Entfernt nach einem gescheiterten Freeze den Marker, den er angelegt hat.
    fn undo_marker(
        &self,
        action: &WardenAction,
        dir: Option<&MarkerDir>,
        target: &CgroupTarget,
        undo: bool,
    ) {
        let Some(dir) = dir.filter(|_| undo) else {
            return;
        };
        if dir.remove(target.kernel_id).is_err() {
            self.degraded(action, "containment marker of a failed freeze not removed");
        }
    }

    // Freigeben (signierter Thaw und Hold-Timer), der Teil vor Ledger, Hold
    // und Marker: gehört das Bit laut Datensatz derselben cgroup nicht dem
    // Warden (beim Eindämmen schon gesetzt, oder der Datensatz ist nicht
    // abgeschlossen: der Warden schreibt das Bit erst nach
    // `freeze_was_set`), gehört der Freeze einem früheren Eigentümer (harw
    // `job.suspend`, ein Operator) und bleibt stehen; die Freigabe wirkt dann
    // nur über ihre Buchung, und die steht zuerst dauerhaft fest (sonst
    // `LedgerUnavailable`, nichts freigegeben). Sonst `cgroup.freeze` = 0
    // und danach die Buchung (ein Absturz dazwischen hinterlässt Bit `0`;
    // scheitert nur das Schreiben, gilt sie im Speicher, und
    // `release_containment` lässt den Marker stehen). Ohne Datensatz nur
    // auftauen — für einen signierten Thaw (der Hold-Timer ruft das nur mit
    // einem Datensatz dieses Boots auf, siehe `expire_one`). Marker und
    // Datensatz gehen erst danach (`release_containment`).
    fn release_freeze(&self, target: &CgroupTarget) -> WardenResult<()> {
        let store = self.state.containment();
        let Some(entry) = store
            .get(target.kernel_id)?
            .filter(|entry| entry.target.path == target.path)
        else {
            return self.thawer.thaw(target);
        };
        if !entry.owns_freeze_bit() {
            return store
                .record_release(target.kernel_id)
                .map(|_| ())
                .map_err(|_| WardenError::LedgerUnavailable {
                    ledger: CONTAINMENT_LEDGER,
                    reason: RELEASE_NOT_DURABLE,
                });
        }
        self.thawer.thaw(target)?;
        // Die Freigabe ist gelaufen; ob ihre Buchung in der Datei steht,
        // prüft `release_containment`, bevor es den Marker entfernt.
        let _ = store.mark_released(target.kernel_id);
        Ok(())
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
        // Der Hold-Timer taut nur eine cgroup auf, die der Warden in diesem
        // Boot selbst eingedämmt hat und noch nicht freigegeben hat: jeder
        // Freeze stellt seinen Datensatz dauerhaft vor dem Hold und vor dem
        // Eingriff. Fehlt ein solcher Datensatz desselben Pfads (Hold aus
        // einem früheren Boot, dessen Kernel-ID jetzt eine neue cgroup trägt;
        // Absturz zwischen Hold und Datensatz in einer älteren Version;
        // Freigabe schon gelaufen), kann ein gesetztes Bit nur einem anderen
        // gehören, etwa harws `job.suspend`: nichts auftauen (fail closed,
        // ein signierter Thaw gibt die cgroup frei), Hold verwerfen.
        match self.state.containment().get(hold.target.kernel_id) {
            Ok(Some(entry)) if entry.target.path == hold.target.path && !entry.released => {}
            Ok(_) => return self.drop_uncontained_hold(hold, &action, now),
            Err(_) => {
                self.audit.record(AuditEvent::Unavailable {
                    action: action.clone(),
                    reason: "containment store unavailable; hold thaw postponed",
                });
                return self.retry_hold(hold, &action, now);
            }
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
        match self.release_freeze(&hold.target) {
            Ok(()) => {
                self.record_hold_outcome(hold, &action, now, LedgerEvent::Executed);
                self.release_containment(&action, &hold.target);
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
            // Die Freigabe eines fremden Bits ließ sich nicht dauerhaft
            // buchen: nichts freigegeben, später erneut versuchen.
            Err(WardenError::LedgerUnavailable {
                ledger: CONTAINMENT_LEDGER,
                reason: RELEASE_NOT_DURABLE,
            }) => {
                self.record_hold_outcome(hold, &action, now, LedgerEvent::Failed);
                self.audit.record(AuditEvent::Unavailable {
                    action: action.clone(),
                    reason: "containment store not writable; hold release postponed",
                });
                self.retry_hold(hold, &action, now)
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

    // Ziel verschwunden oder ersetzt: nichts auftauen, Hold verwerfen. Die
    // cgroup des Holds gibt es nicht mehr; ihre Eindämmung geht mit.
    fn drop_hold(&self, hold: &HoldEntry, action: &WardenAction, now: Timestamp) -> HoldFate {
        self.record_hold_outcome(hold, action, now, LedgerEvent::HoldDropped);
        self.audit.record(AuditEvent::HoldDropped {
            cgroup: hold.target.path.clone(),
        });
        if self.release_containment(action, &hold.target) {
            self.audit.record(AuditEvent::ContainmentCleared {
                cgroup: hold.target.path.clone(),
                kernel_id: hold.target.kernel_id,
                reason: "hold of a cgroup that no longer exists dropped",
            });
        }
        self.forget_hold(
            hold,
            action,
            "hold store not writable; dropped hold removed in memory only",
        );
        HoldFate::Dropped
    }

    // Die cgroup lebt, aber der Warden führt keine (nicht freigegebene)
    // Eindämmung desselben Pfads in diesem Boot (siehe `expire_one`): nichts
    // auftauen, Hold verwerfen. Marker und Datensätze bleiben unberührt: ein
    // Marker ohne Datensatz bleibt fail closed stehen (`reconcile.rs`), einen
    // freigegebenen Datensatz räumt der Durchlauf über die Eindämmungen ab.
    fn drop_uncontained_hold(
        &self,
        hold: &HoldEntry,
        action: &WardenAction,
        now: Timestamp,
    ) -> HoldFate {
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
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};

    use super::{
        CONTAINMENT_SWEEP_SECS, HOLD_RETRY_SECS, HoldSweep, MAX_HOLD_THAW_ATTEMPTS, Warden,
        WardenOutcome, WardenParts,
    };
    use crate::action_ledger::{LedgerEvent, LedgerOrigin, read_verified};
    use crate::audit::{AuditEvent, RecordingAuditSink};
    use crate::containment::{
        CONTAINMENT_FILE, ContainmentEntry, ContainmentMarkers, ContainmentStore, MAX_CONTAINMENTS,
    };
    use crate::error::{WardenError, WardenResult};
    use crate::executor::{
        CgroupFreezer, CgroupThawer, CgroupV2Executor, NetworkIsolator, ProcessTreeKiller,
        RecordedCall, RecordingExecutor,
    };
    use crate::holds::{HOLDS_FILE, HoldEntry};
    use crate::observe::{CgroupFsObserver, StaticTargetObserver};
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

    // Ein Warden über einem privaten Tempdir mit geteilten Aufzeichnungen und
    // einem Indexverzeichnis für die Eindämmungsmarker (`run/holds`).
    struct Fixture {
        dir: tempfile::TempDir,
        run: tempfile::TempDir,
        calls: RecordingExecutor,
        observer: StaticTargetObserver,
        audit: RecordingAuditSink,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let fixture = Self {
                dir: private_tempdir()?,
                run: tempfile::tempdir().map_err(ctx("run dir"))?,
                calls: RecordingExecutor::new(),
                observer: StaticTargetObserver::new(),
                audit: RecordingAuditSink::new(),
            };
            // Wie tmpfiles: dem Warden-Nutzer, `0755` (unabhängig von der umask).
            std::fs::create_dir(fixture.holds()).map_err(ctx("holds dir"))?;
            std::fs::set_permissions(fixture.holds(), std::fs::Permissions::from_mode(0o755))
                .map_err(ctx("chmod holds dir"))?;
            fixture.observer.set(&cgroup(JOB)?, kernel_id(KERNEL_ID)?);
            Ok(fixture)
        }

        // Das Indexverzeichnis der Marker (wie `/run/harw-warden/holds`).
        fn holds(&self) -> PathBuf {
            self.run.path().join("holds")
        }

        // Der Marker der Kernel-ID `raw`.
        fn marker(&self, raw: u64) -> PathBuf {
            self.holds().join(raw.to_string())
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
                markers: ContainmentMarkers::new(self.holds()),
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

        fn freeze_requested(&self, _target: &CgroupTarget) -> WardenResult<bool> {
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

    // Hält beim Lesen des Bits, Einfrieren und Auftauen fest, ob der Marker
    // existiert und ob jemand das Indexverzeichnis exklusiv gesperrt hält.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ProbeStep {
        step: &'static str,
        marker: bool,
        locked: bool,
    }

    #[derive(Clone)]
    struct MarkerProbe {
        holds: PathBuf,
        marker: PathBuf,
        seen: std::sync::Arc<std::sync::Mutex<Vec<ProbeStep>>>,
    }

    impl MarkerProbe {
        fn new(holds: PathBuf, raw: u64) -> Self {
            Self {
                marker: holds.join(raw.to_string()),
                holds,
                seen: std::sync::Arc::default(),
            }
        }

        fn note(&self, step: &'static str) -> WardenResult<()> {
            let index = std::fs::File::open(&self.holds)?;
            let locked = matches!(
                index.try_lock_shared(),
                Err(std::fs::TryLockError::WouldBlock)
            );
            let marker = self.marker.is_file();
            self.seen
                .lock()
                .map_err(|_| WardenError::StatePoisoned)?
                .push(ProbeStep {
                    step,
                    marker,
                    locked,
                });
            Ok(())
        }

        fn seen(&self) -> Vec<ProbeStep> {
            self.seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl CgroupFreezer for MarkerProbe {
        fn freeze(&self, _target: &CgroupTarget) -> WardenResult<()> {
            self.note("freeze")
        }

        fn freeze_requested(&self, _target: &CgroupTarget) -> WardenResult<bool> {
            self.note("read")?;
            Ok(false)
        }
    }

    impl CgroupThawer for MarkerProbe {
        fn thaw(&self, _target: &CgroupTarget) -> WardenResult<()> {
            self.note("thaw")
        }
    }

    // Killer, nach dessen Kill die cgroup sofort verschwunden ist.
    struct VanishingKiller {
        calls: RecordingExecutor,
        observer: StaticTargetObserver,
    }

    impl ProcessTreeKiller for VanishingKiller {
        fn kill(&self, target: &CgroupTarget) -> WardenResult<()> {
            self.observer.remove(&target.path);
            self.calls.kill(target)
        }
    }

    // Gültiger Inhalt; `irreversible_ack` folgt der Aktion (O4).
    fn body(
        action: &WardenAction,
        stage: EscalationStage,
        hold_secs: Option<u32>,
        nonce: u8,
    ) -> TestResult<AuthorizationBody> {
        body_for(action, stage, hold_secs, nonce, KERNEL_ID)
    }

    // Wie `body`, mit einer anderen Kernel-ID (echte Inode einer Fixture-cgroup).
    fn body_for(
        action: &WardenAction,
        stage: EscalationStage,
        hold_secs: Option<u32>,
        nonce: u8,
        kernel: u64,
    ) -> TestResult<AuthorizationBody> {
        Ok(AuthorizationBody {
            finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding id"))?,
            approver: ApprovalActor::Operator {
                id: "operator-1".to_owned(),
            },
            action: action.clone(),
            stage,
            target: CgroupTarget::new(action.cgroup().clone(), kernel_id(kernel)?, at(-3600)?),
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

    fn request_for(
        action: &WardenAction,
        stage: EscalationStage,
        nonce: u8,
        kernel: u64,
    ) -> TestResult<WardenRequest> {
        signed(
            KEY_OLD,
            "k-old",
            action,
            body_for(action, stage, None, nonce, kernel)?,
        )
    }

    fn freeze() -> TestResult<WardenAction> {
        Ok(WardenAction::FreezeCgroup {
            cgroup: cgroup(JOB)?,
        })
    }

    fn thaw() -> TestResult<WardenAction> {
        Ok(WardenAction::ThawCgroup {
            cgroup: cgroup(JOB)?,
        })
    }

    fn executed(outcome: &WardenOutcome) -> bool {
        matches!(outcome, WardenOutcome::Executed(_))
    }

    // Die festen Gründe aller `ContainmentDegraded`-Einträge, in Reihenfolge.
    fn degraded_reasons(fixture: &Fixture) -> Vec<&'static str> {
        fixture
            .audit
            .events()
            .iter()
            .filter_map(|event| match event {
                AuditEvent::ContainmentDegraded { reason, .. } => Some(*reason),
                _ => None,
            })
            .collect()
    }

    fn cleared_count(fixture: &Fixture) -> usize {
        fixture
            .audit
            .events()
            .iter()
            .filter(|event| matches!(event, AuditEvent::ContainmentCleared { .. }))
            .count()
    }

    fn containments(warden: &Warden) -> TestResult<Vec<ContainmentEntry>> {
        warden
            .state()
            .containment()
            .snapshot()
            .map_err(ctx("containment snapshot"))
    }

    // Eine Fixture-cgroup `<root>/harw.slice/job-1` mit `cgroup.freeze` =
    // `bit`; liefert ihre Inode (= Kernel-ID).
    fn fixture_cgroup(root: &Path, bit: &[u8]) -> TestResult<u64> {
        let job = root.join(JOB);
        std::fs::create_dir_all(&job).map_err(ctx("create cgroup dir"))?;
        std::fs::write(job.join("cgroup.freeze"), bit).map_err(ctx("write cgroup.freeze"))?;
        Ok(std::fs::metadata(&job).map_err(ctx("metadata"))?.ino())
    }

    // Warden mit echter cgroup-v2-Ausführung und Beobachtung unter `root`.
    fn real_cgroup_warden(fixture: &Fixture, root: &Path) -> TestResult<Warden> {
        let mut parts = fixture.default_parts()?;
        parts.freezer = Box::new(CgroupV2Executor::new(root));
        parts.thawer = Box::new(CgroupV2Executor::new(root));
        parts.killer = Box::new(CgroupV2Executor::new(root));
        parts.observer = Box::new(CgroupFsObserver::new(root));
        Ok(Warden::new(parts))
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
        // Der Freeze eines ersetzten Ziels nimmt Marker und Datensatz zurück.
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
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

    // -- Eindämmungsmarker für harw (Owner-Review 4505015305) -------------------

    #[test]
    fn test_marker_comes_before_the_freeze_and_goes_after_the_thaw() -> TestResult {
        let fixture = Fixture::new()?;
        let probe = MarkerProbe::new(fixture.holds(), KERNEL_ID);
        let mut parts = fixture.default_parts()?;
        parts.freezer = Box::new(probe.clone());
        parts.thawer = Box::new(probe.clone());
        let warden = Warden::new(parts);
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(fixture.marker(KERNEL_ID).is_file());
        // Nach dem Freeze ist die Sperre wieder frei.
        assert!(
            ContainmentMarkers::new(fixture.holds())
                .open_locked()
                .map_err(ctx("lock after freeze"))?
                .is_locked()
        );
        let record = warden
            .state()
            .containment()
            .get(kernel_id(KERNEL_ID)?)
            .map_err(ctx("record"))?
            .ok_or(TestError::Missing("containment record"))?;
        assert_eq!(record.freeze_was_set, Some(false));
        assert!(executed(&warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?
        )));
        // Marker vor dem Lesen und Einfrieren (unter der Sperre); beim
        // Auftauen noch da, erst danach entfernt.
        assert_eq!(
            probe.seen(),
            vec![
                ProbeStep {
                    step: "read",
                    marker: true,
                    locked: true
                },
                ProbeStep {
                    step: "freeze",
                    marker: true,
                    locked: true
                },
                ProbeStep {
                    step: "thaw",
                    marker: true,
                    locked: false
                },
            ]
        );
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        assert!(degraded_reasons(&fixture).is_empty());
        Ok(())
    }

    #[test]
    fn test_release_keeps_a_freeze_bit_that_was_set_before_the_containment() -> TestResult {
        let fixture = Fixture::new()?;
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        // harw hat den Job vorher angehalten (`job.suspend`).
        let inode = fixture_cgroup(root.path(), b"1\n")?;
        let control = root.path().join(JOB).join("cgroup.freeze");
        let warden = real_cgroup_warden(&fixture, root.path())?;
        assert!(executed(&warden.handle(
            &request_for(&freeze()?, EscalationStage::RuleTriggered, 1, inode)?,
            at(1)?
        )));
        assert!(fixture.marker(inode).is_file());
        let record = warden
            .state()
            .containment()
            .get(kernel_id(inode)?)
            .map_err(ctx("record"))?
            .ok_or(TestError::Missing("containment record"))?;
        assert_eq!(record.freeze_was_set, Some(true));
        assert!(executed(&warden.handle(
            &request_for(&thaw()?, EscalationStage::RuleTriggered, 2, inode)?,
            at(2)?
        )));
        // harws Freeze bleibt stehen; nur der Marker des Wardens ist weg.
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "1"
        );
        assert!(!fixture.marker(inode).exists());
        assert!(containments(&warden)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_release_thaws_its_own_freeze_even_after_a_refreeze() -> TestResult {
        let fixture = Fixture::new()?;
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        let inode = fixture_cgroup(root.path(), b"0\n")?;
        let control = root.path().join(JOB).join("cgroup.freeze");
        let warden = real_cgroup_warden(&fixture, root.path())?;
        for nonce in [1u8, 2u8] {
            // Der zweite Freeze liest das eigene Bit (`1`) und darf es nicht
            // als fremden Freeze festhalten.
            assert!(executed(&warden.handle(
                &request_for(&freeze()?, EscalationStage::RuleTriggered, nonce, inode)?,
                at(1)?
            )));
            assert_eq!(
                std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
                "1"
            );
        }
        let record = warden
            .state()
            .containment()
            .get(kernel_id(inode)?)
            .map_err(ctx("record"))?
            .ok_or(TestError::Missing("containment record"))?;
        assert_eq!(record.freeze_was_set, Some(false));
        assert!(executed(&warden.handle(
            &request_for(&thaw()?, EscalationStage::RuleTriggered, 3, inode)?,
            at(2)?
        )));
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "0"
        );
        assert!(!fixture.marker(inode).exists());
        assert!(containments(&warden)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_hold_expiry_keeps_a_preset_freeze_bit_and_removes_the_marker() -> TestResult {
        let fixture = Fixture::new()?;
        let mut parts = fixture.default_parts()?;
        parts.freezer = Box::new(fixture.calls.reporting_frozen());
        let warden = Warden::new(parts);
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
            at(1)?
        )));
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(warden.expire_holds(at(61)?).thawed, 1);
        // Kein Auftauen: das Bit gehörte schon vorher einem anderen.
        assert_eq!(
            fixture.calls.calls(),
            vec![RecordedCall::Freeze(cgroup(JOB)?)]
        );
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_symlinked_index_still_freezes_without_a_marker_and_reports_it() -> TestResult {
        let fixture = Fixture::new()?;
        let real = fixture.run.path().join("real");
        std::fs::rename(fixture.holds(), &real).map_err(ctx("move index"))?;
        std::os::unix::fs::symlink(&real, fixture.holds()).map_err(ctx("symlink index"))?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert_eq!(
            fixture.calls.calls(),
            vec![RecordedCall::Freeze(cgroup(JOB)?)]
        );
        assert_eq!(
            std::fs::read_dir(&real).map_err(ctx("read dir"))?.count(),
            0
        );
        assert_eq!(
            degraded_reasons(&fixture),
            ["containment index unusable; freezing without a marker"]
        );
        // `freeze_was_set` ist trotzdem festgehalten (für die Freigabe).
        let record = warden
            .state()
            .containment()
            .get(kernel_id(KERNEL_ID)?)
            .map_err(ctx("record"))?
            .ok_or(TestError::Missing("containment record"))?;
        assert_eq!(record.freeze_was_set, Some(false));
        Ok(())
    }

    #[test]
    fn test_missing_index_still_freezes_and_the_thaw_releases_the_record() -> TestResult {
        let fixture = Fixture::new()?;
        std::fs::remove_dir(fixture.holds()).map_err(ctx("remove index"))?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert_eq!(containments(&warden)?.len(), 1);
        assert!(executed(&warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?
        )));
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Thaw(cgroup(JOB)?)
            ]
        );
        // Ohne Index gibt es keinen Marker, der bleiben müsste.
        assert!(containments(&warden)?.is_empty());
        assert_eq!(
            degraded_reasons(&fixture),
            ["containment index unusable; freezing without a marker"]
        );
        Ok(())
    }

    #[test]
    fn test_symlinked_marker_is_never_followed_or_removed() -> TestResult {
        let fixture = Fixture::new()?;
        let outside = fixture.run.path().join("outside");
        std::fs::write(&outside, b"foreign").map_err(ctx("write outside"))?;
        std::os::unix::fs::symlink(&outside, fixture.marker(KERNEL_ID))
            .map_err(ctx("symlink marker"))?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(executed(&warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?
        )));
        assert_eq!(
            degraded_reasons(&fixture),
            [
                "containment marker not created; freezing without it",
                "containment marker not removed after the release; the hold timer retries, job.resume stays refused until then",
            ]
        );
        assert_eq!(
            std::fs::read(&outside).map_err(ctx("read outside"))?,
            b"foreign"
        );
        assert!(
            std::fs::symlink_metadata(fixture.marker(KERNEL_ID))
                .map_err(ctx("lstat marker"))?
                .file_type()
                .is_symlink()
        );
        // Der Datensatz bleibt, solange der Marker nicht entfernt ist, als
        // freigegeben; der Durchlauf des Hold-Timers folgt dem Symlink nie.
        let records = containments(&warden)?;
        assert_eq!(records.len(), 1);
        assert!(records.iter().all(|entry| entry.released));
        assert_eq!(warden.expire_holds(at(30)?).cleared, 0);
        assert!(
            std::fs::symlink_metadata(fixture.marker(KERNEL_ID))
                .map_err(ctx("lstat marker again"))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(containments(&warden)?.len(), 1);
        Ok(())
    }

    #[test]
    fn test_an_existing_own_marker_is_taken_over_and_released() -> TestResult {
        let fixture = Fixture::new()?;
        std::fs::write(fixture.marker(KERNEL_ID), b"").map_err(ctx("old marker"))?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert!(executed(&warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?
        )));
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(degraded_reasons(&fixture).is_empty());
        Ok(())
    }

    #[test]
    fn test_contended_index_lock_does_not_stop_the_containment() -> TestResult {
        let fixture = Fixture::new()?;
        // Jeder lokale Nutzer kann das Verzeichnis sperren.
        let other = std::fs::File::open(fixture.holds()).map_err(ctx("open index"))?;
        other.lock().map_err(ctx("lock index"))?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(
            fixture.calls.calls(),
            vec![RecordedCall::Freeze(cgroup(JOB)?)]
        );
        assert_eq!(
            degraded_reasons(&fixture),
            ["containment index lock not obtained; containing without it"]
        );
        Ok(())
    }

    #[test]
    fn test_failed_freeze_takes_back_its_marker_and_record() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden_with(
            ring(&[("k-old", KEY_OLD)])?,
            all_actions_policy(&["harw.slice"], DenyList::builtin())?,
            fixture.calls.failing_view(),
        )?;
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?,
        );
        assert_eq!(outcome, WardenOutcome::ExecutionFailed);
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_unwritable_containment_store_executes_nothing() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        // Jedes Schreiben des Eindämmungsspeichers scheitert (`O_NOFOLLOW`).
        std::os::unix::fs::symlink(
            fixture.dir.path().join("elsewhere"),
            fixture.dir.path().join(format!("{CONTAINMENT_FILE}.tmp")),
        )
        .map_err(ctx("block the containment store"))?;
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
            at(1)?,
        );
        assert_eq!(outcome, WardenOutcome::Unavailable);
        assert!(fixture.calls.calls().is_empty());
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(
            warden
                .state()
                .holds()
                .snapshot()
                .map_err(ctx("holds"))?
                .is_empty()
        );
        // Datensatz vor Hold: der Hold wurde gar nicht erst gestellt.
        assert!(!fixture.dir.path().join(HOLDS_FILE).exists());
        assert_eq!(
            ledger_events(&fixture)?,
            [LedgerEvent::Attempt, LedgerEvent::Failed]
        );
        assert!(matches!(
            fixture.audit.events().last(),
            Some(AuditEvent::Unavailable {
                reason: "containment store not writable; nothing executed",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn test_signed_thaw_of_a_vanished_cgroup_clears_its_stale_containment() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        fixture.observer.remove(&cgroup(JOB)?);
        let cleared = warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?,
        );
        assert!(matches!(cleared.to_wire(), WardenResponse::Executed { .. }));
        // Nichts aufgetaut: es gibt keine cgroup mehr; nur Marker und Datensatz.
        assert_eq!(
            fixture.calls.calls(),
            vec![RecordedCall::Freeze(cgroup(JOB)?)]
        );
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        assert_eq!(cleared_count(&fixture), 1);
        // Das Ledger nennt den Operator-Pfad: nichts aufgetaut.
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Executed,
                LedgerEvent::Attempt,
                LedgerEvent::StaleContainmentCleared
            ]
        );
        // Danach ist nichts mehr abzuräumen: wie bisher `TargetChanged`.
        let again = warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 3)?,
            at(2)?,
        );
        assert_eq!(denied(&again), Some(Denial::TargetChanged));
        Ok(())
    }

    #[test]
    fn test_signed_thaw_clears_an_orphan_marker_of_a_replaced_cgroup() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        std::fs::write(fixture.marker(KERNEL_ID), b"").map_err(ctx("orphan marker"))?;
        fixture
            .observer
            .set(&cgroup(JOB)?, kernel_id(KERNEL_ID.saturating_add(1))?);
        let cleared = warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?,
        );
        assert!(executed(&cleared));
        assert!(fixture.calls.calls().is_empty());
        assert!(!fixture.marker(KERNEL_ID).exists());
        Ok(())
    }

    #[test]
    fn test_signed_thaw_never_clears_a_marker_of_another_containment() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        // Dieselbe Kernel-ID gehört einer Eindämmung unter einem anderen Pfad.
        warden
            .state()
            .containment()
            .record(ContainmentEntry {
                target: CgroupTarget::new(
                    cgroup("harw.slice/job-2")?,
                    kernel_id(KERNEL_ID)?,
                    at(-60)?,
                ),
                freeze_was_set: Some(false),
                released: false,
            })
            .map_err(ctx("other record"))?;
        std::fs::write(fixture.marker(KERNEL_ID), b"").map_err(ctx("other marker"))?;
        fixture.observer.remove(&cgroup(JOB)?);
        let outcome = warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?,
        );
        assert_eq!(denied(&outcome), Some(Denial::TargetChanged));
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(containments(&warden)?.len(), 1);
        Ok(())
    }

    #[test]
    fn test_dropped_hold_of_a_vanished_cgroup_clears_its_containment() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
            at(1)?
        )));
        fixture.observer.remove(&cgroup(JOB)?);
        assert_eq!(warden.expire_holds(at(61)?).dropped, 1);
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        assert_eq!(cleared_count(&fixture), 1);
        Ok(())
    }

    #[test]
    fn test_kill_keeps_the_containment_while_the_cgroup_exists() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let kill = WardenAction::KillProcessTree {
            cgroup: cgroup(JOB)?,
        };
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(executed(&warden.handle(
            &request(&kill, EscalationStage::Escalated, None, 2)?,
            at(1)?
        )));
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(containments(&warden)?.len(), 1);
        assert_eq!(cleared_count(&fixture), 0);
        Ok(())
    }

    #[test]
    fn test_kill_of_a_cgroup_that_vanished_clears_its_containment() -> TestResult {
        let fixture = Fixture::new()?;
        let mut parts = fixture.default_parts()?;
        parts.killer = Box::new(VanishingKiller {
            calls: fixture.calls.clone(),
            observer: fixture.observer.clone(),
        });
        let warden = Warden::new(parts);
        let kill = WardenAction::KillProcessTree {
            cgroup: cgroup(JOB)?,
        };
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(executed(&warden.handle(
            &request(&kill, EscalationStage::Escalated, None, 2)?,
            at(1)?
        )));
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        assert_eq!(cleared_count(&fixture), 1);
        Ok(())
    }

    // -- Abgleich beim Start ------------------------------------------------------

    #[test]
    fn test_restart_reconciliation_clears_a_marker_left_between_thaw_and_unlink() -> TestResult {
        let fixture = Fixture::new()?;
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        let inode = fixture_cgroup(root.path(), b"0\n")?;
        let control = root.path().join(JOB).join("cgroup.freeze");
        {
            let warden = real_cgroup_warden(&fixture, root.path())?;
            assert!(executed(&warden.handle(
                &request_for(&freeze()?, EscalationStage::RuleTriggered, 1, inode)?,
                at(1)?
            )));
        }
        assert!(fixture.marker(inode).is_file());
        // Absturz nach `cgroup.freeze` = 0 und vor dem Entfernen des Markers.
        std::fs::write(&control, b"0").map_err(ctx("thaw before the crash"))?;

        // Neustart: ein neuer Warden über demselben StateDirectory.
        let restarted = real_cgroup_warden(&fixture, root.path())?;
        let report = restarted
            .reconcile_containments()
            .map_err(ctx("reconcile"))?;
        assert_eq!(report.thawed, 1);
        assert_eq!(report.kept, 0);
        assert!(!fixture.marker(inode).exists());
        assert!(containments(&restarted)?.is_empty());
        assert_eq!(cleared_count(&fixture), 1);
        // Der Abgleich hat `cgroup.freeze` nicht angefasst.
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "0"
        );
        Ok(())
    }

    #[test]
    fn test_restart_reconciliation_keeps_a_frozen_containment() -> TestResult {
        let fixture = Fixture::new()?;
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        let inode = fixture_cgroup(root.path(), b"0\n")?;
        let control = root.path().join(JOB).join("cgroup.freeze");
        {
            let warden = real_cgroup_warden(&fixture, root.path())?;
            assert!(executed(&warden.handle(
                &request_for(&freeze()?, EscalationStage::RuleTriggered, 1, inode)?,
                at(1)?
            )));
        }
        let restarted = real_cgroup_warden(&fixture, root.path())?;
        let report = restarted
            .reconcile_containments()
            .map_err(ctx("reconcile"))?;
        assert_eq!(report.kept, 1);
        assert_eq!(report.restored, 0);
        assert!(report.index_locked);
        assert!(fixture.marker(inode).is_file());
        // Die Freigabe nach dem Neustart taut ihren eigenen Freeze auf und
        // entfernt erst danach den Marker.
        assert!(executed(&restarted.handle(
            &request_for(&thaw()?, EscalationStage::RuleTriggered, 2, inode)?,
            at(2)?
        )));
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "0"
        );
        assert!(!fixture.marker(inode).exists());
        assert!(containments(&restarted)?.is_empty());
        Ok(())
    }

    // -- Runde 2: Durchlauf, Freigabe-Markierung, fremdes Bit ---------------------

    // Ein zweiter Job mit eigener Kernel-ID (für Tests mit zwei cgroups).
    const JOB_2: &str = "harw.slice/job-2";
    const KERNEL_ID_2: u64 = 4343;

    fn freeze_of(path: &str) -> TestResult<WardenAction> {
        Ok(WardenAction::FreezeCgroup {
            cgroup: cgroup(path)?,
        })
    }

    // Schreibt einen Eindämmungsspeicher mit `MAX_CONTAINMENTS` Datensätzen
    // (`harw.slice/old-<n>`, Kernel-IDs ab 10 000) ins StateDirectory, bevor
    // der Warden ihn öffnet; liefert die Pfade.
    fn fill_containment_store(fixture: &Fixture) -> TestResult<Vec<CgroupId>> {
        let mut paths = Vec::new();
        let mut entries = Vec::new();
        for index in 0..MAX_CONTAINMENTS {
            let raw = u64::try_from(index)
                .map_err(ctx("index"))?
                .saturating_add(10_000);
            let path = cgroup(&format!("harw.slice/old-{raw}"))?;
            entries.push(
                serde_json::to_value(ContainmentEntry {
                    target: CgroupTarget::new(path.clone(), kernel_id(raw)?, at(-60)?),
                    freeze_was_set: Some(false),
                    released: false,
                })
                .map_err(ctx("encode record"))?,
            );
            paths.push(path);
        }
        let file = serde_json::json!({ "version": 1, "containments": entries });
        std::fs::write(
            fixture.dir.path().join(CONTAINMENT_FILE),
            serde_json::to_vec(&file).map_err(ctx("encode store"))?,
        )
        .map_err(ctx("write store"))?;
        Ok(paths)
    }

    #[test]
    fn test_sweep_clears_the_containment_of_a_killed_cgroup_once_it_is_gone() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        let kill = WardenAction::KillProcessTree {
            cgroup: cgroup(JOB)?,
        };
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(executed(&warden.handle(
            &request(&kill, EscalationStage::Escalated, None, 2)?,
            at(1)?
        )));
        // `cgroup.kill` lässt das Verzeichnis stehen: alles bleibt.
        assert_eq!(warden.expire_holds(at(2)?).cleared, 0);
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(containments(&warden)?.len(), 1);
        // Später entfernt systemd die cgroup; der nächste Durchlauf räumt ab.
        fixture.observer.remove(&cgroup(JOB)?);
        let sweep = warden.expire_holds(at(2_i64.saturating_add(CONTAINMENT_SWEEP_SECS))?);
        assert_eq!(sweep.cleared, 1);
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        assert_eq!(cleared_count(&fixture), 1);
        // Der Durchlauf greift in keine cgroup ein und schreibt nichts ins Ledger.
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Kill(cgroup(JOB)?)
            ]
        );
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Executed,
                LedgerEvent::Attempt,
                LedgerEvent::Executed
            ]
        );
        Ok(())
    }

    #[test]
    fn test_sweep_runs_at_most_every_interval_and_after_a_clock_jump_back() -> TestResult {
        let fixture = Fixture::new()?;
        fixture
            .observer
            .set(&cgroup(JOB_2)?, kernel_id(KERNEL_ID_2)?);
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(executed(&warden.handle(
            &request_for(
                &freeze_of(JOB_2)?,
                EscalationStage::RuleTriggered,
                2,
                KERNEL_ID_2
            )?,
            at(2)?
        )));
        // Erster Durchlauf: alles lebt.
        assert_eq!(warden.expire_holds(at(3)?).cleared, 0);
        fixture.observer.remove(&cgroup(JOB)?);
        // Innerhalb des Abstands kein Durchlauf.
        assert_eq!(warden.expire_holds(at(4)?).cleared, 0);
        assert_eq!(containments(&warden)?.len(), 2);
        let due = 3_i64.saturating_add(CONTAINMENT_SWEEP_SECS);
        assert_eq!(warden.expire_holds(at(due)?).cleared, 1);
        assert!(!fixture.marker(KERNEL_ID).exists());
        // Die Uhr springt zurück: sofort ein Durchlauf.
        fixture.observer.remove(&cgroup(JOB_2)?);
        assert_eq!(warden.expire_holds(at(5)?).cleared, 1);
        assert!(!fixture.marker(KERNEL_ID_2).exists());
        assert!(containments(&warden)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_full_store_is_swept_before_a_freeze_and_then_records_it() -> TestResult {
        let fixture = Fixture::new()?;
        fill_containment_store(&fixture)?;
        // Keine der alten cgroups gibt es noch (der Beobachter kennt nur JOB).
        let warden = fixture.warden()?;
        assert_eq!(containments(&warden)?.len(), MAX_CONTAINMENTS);
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        let records = containments(&warden)?;
        assert_eq!(records.len(), 1);
        assert!(
            records
                .iter()
                .all(|entry| entry.target.kernel_id.get() == KERNEL_ID)
        );
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(cleared_count(&fixture), MAX_CONTAINMENTS);
        Ok(())
    }

    #[test]
    fn test_store_full_of_live_containments_refuses_with_its_own_reason() -> TestResult {
        let fixture = Fixture::new()?;
        let paths = fill_containment_store(&fixture)?;
        for (offset, path) in (10_000_u64..).zip(&paths) {
            fixture.observer.set(path, kernel_id(offset)?);
        }
        let warden = fixture.warden()?;
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
            at(1)?,
        );
        assert_eq!(outcome, WardenOutcome::Unavailable);
        assert!(fixture.calls.calls().is_empty());
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert_eq!(containments(&warden)?.len(), MAX_CONTAINMENTS);
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
            Some(AuditEvent::Unavailable {
                reason: "containment store full of containments of live cgroups; nothing executed",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn test_marker_left_by_a_release_is_retried_by_the_sweep() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        // Das Indexverzeichnis ist vorübergehend nicht vertrauenswürdig.
        std::fs::set_permissions(fixture.holds(), std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("chmod 0777"))?;
        assert!(executed(&warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?
        )));
        // Aufgetaut, aber der Marker liegt noch: Datensatz als freigegeben.
        assert!(fixture.marker(KERNEL_ID).is_file());
        let records = containments(&warden)?;
        assert_eq!(records.len(), 1);
        assert!(
            records
                .iter()
                .all(|entry| entry.released && !entry.is_active())
        );
        // Solange der Index unbrauchbar ist, tut der Durchlauf nichts.
        assert_eq!(warden.expire_holds(at(3)?).cleared, 0);
        assert!(fixture.marker(KERNEL_ID).is_file());
        std::fs::set_permissions(fixture.holds(), std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod 0755"))?;
        let sweep = warden.expire_holds(at(3_i64.saturating_add(CONTAINMENT_SWEEP_SECS))?);
        assert_eq!(sweep.cleared, 1);
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        // Genau ein Thaw: der Durchlauf schreibt nie `cgroup.freeze`.
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Thaw(cgroup(JOB)?)
            ]
        );
        Ok(())
    }

    #[test]
    fn test_refreeze_owns_the_bit_once_the_earlier_owner_lifted_it() -> TestResult {
        let fixture = Fixture::new()?;
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        // harw hat den Job angehalten (`job.suspend`).
        let inode = fixture_cgroup(root.path(), b"1\n")?;
        let control = root.path().join(JOB).join("cgroup.freeze");
        let warden = real_cgroup_warden(&fixture, root.path())?;
        assert!(executed(&warden.handle(
            &request_for(&freeze()?, EscalationStage::RuleTriggered, 1, inode)?,
            at(1)?
        )));
        let record = |warden: &Warden| -> TestResult<ContainmentEntry> {
            warden
                .state()
                .containment()
                .get(kernel_id(inode)?)
                .map_err(ctx("record"))?
                .ok_or(TestError::Missing("containment record"))
        };
        assert_eq!(record(&warden)?.freeze_was_set, Some(true));
        // Ein Operator hebt harws Freeze außerhalb von harw auf
        // (`systemctl thaw`); der Warden friert erneut ein und schreibt das
        // Bit damit selbst.
        std::fs::write(&control, b"0").map_err(ctx("thaw outside"))?;
        assert!(executed(&warden.handle(
            &request_for(&freeze()?, EscalationStage::RuleTriggered, 2, inode)?,
            at(2)?
        )));
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "1"
        );
        assert_eq!(record(&warden)?.freeze_was_set, Some(false));
        // Die Freigabe taut deshalb auf.
        assert!(executed(&warden.handle(
            &request_for(&thaw()?, EscalationStage::RuleTriggered, 3, inode)?,
            at(3)?
        )));
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "0"
        );
        assert!(!fixture.marker(inode).exists());
        assert!(containments(&warden)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_refreeze_keeps_a_foreign_bit_that_is_still_set() -> TestResult {
        let fixture = Fixture::new()?;
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        let inode = fixture_cgroup(root.path(), b"1\n")?;
        let control = root.path().join(JOB).join("cgroup.freeze");
        let warden = real_cgroup_warden(&fixture, root.path())?;
        for nonce in [1u8, 2u8] {
            assert!(executed(&warden.handle(
                &request_for(&freeze()?, EscalationStage::RuleTriggered, nonce, inode)?,
                at(1)?
            )));
        }
        let entry = warden
            .state()
            .containment()
            .get(kernel_id(inode)?)
            .map_err(ctx("record"))?
            .ok_or(TestError::Missing("containment record"))?;
        assert_eq!(entry.freeze_was_set, Some(true));
        assert!(executed(&warden.handle(
            &request_for(&thaw()?, EscalationStage::RuleTriggered, 3, inode)?,
            at(2)?
        )));
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "1"
        );
        assert!(!fixture.marker(inode).exists());
        Ok(())
    }

    #[test]
    fn test_operator_clear_with_an_unremovable_marker_fails_and_keeps_the_record() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        // Unter dem Markernamen liegt jetzt ein Symlink nach draußen.
        let outside = fixture.run.path().join("outside");
        std::fs::write(&outside, b"foreign").map_err(ctx("write outside"))?;
        std::fs::remove_file(fixture.marker(KERNEL_ID)).map_err(ctx("remove marker"))?;
        std::os::unix::fs::symlink(&outside, fixture.marker(KERNEL_ID))
            .map_err(ctx("symlink marker"))?;
        fixture.observer.remove(&cgroup(JOB)?);
        let outcome = warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?,
        );
        assert_eq!(outcome, WardenOutcome::ExecutionFailed);
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Executed,
                LedgerEvent::Attempt,
                LedgerEvent::Failed
            ]
        );
        assert_eq!(containments(&warden)?.len(), 1);
        assert!(
            std::fs::symlink_metadata(fixture.marker(KERNEL_ID))
                .map_err(ctx("lstat marker"))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(&outside).map_err(ctx("read outside"))?,
            b"foreign"
        );
        assert_eq!(cleared_count(&fixture), 0);
        // Nur die Eindämmung, nie eine cgroup.
        assert_eq!(
            fixture.calls.calls(),
            vec![RecordedCall::Freeze(cgroup(JOB)?)]
        );
        Ok(())
    }

    #[test]
    fn test_vanished_thaw_with_an_uncheckable_marker_is_denied_without_degraded() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        fixture.observer.remove(&cgroup(JOB)?);
        // Ohne Indexverzeichnis gibt es keinen Marker: still `TargetChanged`.
        std::fs::remove_dir(fixture.holds()).map_err(ctx("remove index"))?;
        let missing = warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?,
        );
        assert_eq!(denied(&missing), Some(Denial::TargetChanged));
        assert!(fixture.audit.events().iter().all(|event| !matches!(
            event,
            AuditEvent::ContainmentUnresolved { .. } | AuditEvent::ContainmentDegraded { .. }
        )));
        // Ein Symlink unter dem Markernamen: nicht prüfbar, nichts angefasst,
        // gemeldet als ungeklärt (nicht als „Eingriff lief weiter“).
        std::fs::create_dir(fixture.holds()).map_err(ctx("index again"))?;
        std::fs::set_permissions(fixture.holds(), std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod index"))?;
        let outside = fixture.run.path().join("outside");
        std::fs::write(&outside, b"foreign").map_err(ctx("write outside"))?;
        std::os::unix::fs::symlink(&outside, fixture.marker(KERNEL_ID))
            .map_err(ctx("symlink marker"))?;
        let linked = warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(1)?,
        );
        assert_eq!(denied(&linked), Some(Denial::TargetChanged));
        assert!(degraded_reasons(&fixture).is_empty());
        assert_eq!(
            fixture
                .audit
                .events()
                .iter()
                .filter(|event| matches!(event, AuditEvent::ContainmentUnresolved { .. }))
                .count(),
            1
        );
        assert!(
            std::fs::symlink_metadata(fixture.marker(KERNEL_ID))
                .map_err(ctx("lstat marker"))?
                .file_type()
                .is_symlink()
        );
        assert!(ledger_events(&fixture)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_thaw_after_restart_never_lifts_the_bit_of_an_unfinished_containment() -> TestResult {
        let fixture = Fixture::new()?;
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        // harw hat den Job angehalten; der Warden stürzte nach dem Marker und
        // vor dem dauerhaften `freeze_was_set` ab.
        let inode = fixture_cgroup(root.path(), b"1\n")?;
        let control = root.path().join(JOB).join("cgroup.freeze");
        {
            let crashed = real_cgroup_warden(&fixture, root.path())?;
            crashed
                .state()
                .containment()
                .record(ContainmentEntry::containing(CgroupTarget::new(
                    cgroup(JOB)?,
                    kernel_id(inode)?,
                    at(-3600)?,
                )))
                .map_err(ctx("record"))?;
            ContainmentMarkers::new(fixture.holds())
                .open()
                .map_err(ctx("open index"))?
                .create(kernel_id(inode)?)
                .map_err(ctx("marker"))?;
        }
        let restarted = real_cgroup_warden(&fixture, root.path())?;
        let report = restarted
            .reconcile_containments()
            .map_err(ctx("reconcile"))?;
        assert_eq!(report.completed, 1);
        assert_eq!(report.kept, 1);
        assert!(fixture.marker(inode).is_file());
        assert!(executed(&restarted.handle(
            &request_for(&thaw()?, EscalationStage::RuleTriggered, 1, inode)?,
            at(1)?
        )));
        // harws Freeze bleibt; nur Marker und Datensatz des Wardens gehen.
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "1\n"
        );
        assert!(!fixture.marker(inode).exists());
        assert!(containments(&restarted)?.is_empty());
        Ok(())
    }

    // -- Runde 3: Hold ohne Eindämmung, Buchung vor dem Eingriff ----------------

    // Ein Hold für `JOB` mit `KERNEL_ID`, fällig bei `at(60)`.
    fn job_hold() -> TestResult<HoldEntry> {
        Ok(HoldEntry {
            target: CgroupTarget::new(cgroup(JOB)?, kernel_id(KERNEL_ID)?, at(-3600)?),
            thaw_at: at(60)?,
            finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding id"))?,
            nonce: "5a".repeat(16),
            attempts: 0,
        })
    }

    fn holds_of(warden: &Warden) -> TestResult<Vec<HoldEntry>> {
        warden.state().holds().snapshot().map_err(ctx("holds"))
    }

    // Die Datensätze, wie sie in der Datei stehen (wie nach einem Neustart).
    fn stored_containments(fixture: &Fixture) -> TestResult<Vec<ContainmentEntry>> {
        ContainmentStore::open(fixture.dir.path())
            .map_err(ctx("reopen containment store"))?
            .snapshot()
            .map_err(ctx("stored containments"))
    }

    // Freezer, dessen Lesen des Bits auf Wunsch zuerst den
    // Eindämmungsspeicher unschreibbar macht (Symlink auf der Zwischendatei,
    // `O_NOFOLLOW`): wie ein Datenträger, der zwischen dem Datensatz und
    // `freeze_was_set` voll läuft. Das gemeldete Bit ist einstellbar;
    // Einfrieren zeichnet nur auf und scheitert auf Wunsch danach (I/O-Fehler,
    // kein geändertes Ziel).
    #[derive(Clone)]
    struct StoreBlockingFreezer {
        calls: RecordingExecutor,
        state_dir: PathBuf,
        was_set: std::sync::Arc<std::sync::atomic::AtomicBool>,
        block: std::sync::Arc<std::sync::atomic::AtomicBool>,
        fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl StoreBlockingFreezer {
        fn new(fixture: &Fixture, was_set: bool, block: bool) -> Self {
            Self {
                calls: fixture.calls.clone(),
                state_dir: fixture.dir.path().to_path_buf(),
                was_set: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(was_set)),
                block: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(block)),
                fail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }

        fn set(&self, was_set: bool, block: bool) {
            self.was_set
                .store(was_set, std::sync::atomic::Ordering::SeqCst);
            self.block.store(block, std::sync::atomic::Ordering::SeqCst);
        }

        fn fail_freezes(&self, fail: bool) {
            self.fail.store(fail, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl CgroupFreezer for StoreBlockingFreezer {
        fn freeze(&self, target: &CgroupTarget) -> WardenResult<()> {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return self.calls.failing_view().freeze(target);
            }
            self.calls.freeze(target)
        }

        fn freeze_requested(&self, _target: &CgroupTarget) -> WardenResult<bool> {
            if self.block.load(std::sync::atomic::Ordering::SeqCst) {
                let tmp = self.state_dir.join(format!("{CONTAINMENT_FILE}.tmp"));
                if std::fs::symlink_metadata(&tmp).is_err() {
                    std::os::unix::fs::symlink(self.state_dir.join("elsewhere"), &tmp)?;
                }
            }
            Ok(self.was_set.load(std::sync::atomic::Ordering::SeqCst))
        }
    }

    #[test]
    fn test_due_hold_without_a_containment_of_this_boot_is_dropped_without_a_thaw() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        // Ein Hold ohne Eindämmungsdatensatz (aus einem früheren Boot, dessen
        // Kernel-ID jetzt eine neue cgroup unter demselben Pfad trägt, die
        // etwa harw angehalten hat). Ein Marker gleichen Namens ohne
        // Datensatz bleibt fail closed stehen.
        warden
            .state()
            .holds()
            .arm(job_hold()?)
            .map_err(ctx("arm hold"))?;
        std::fs::write(fixture.marker(KERNEL_ID), b"").map_err(ctx("orphan marker"))?;
        let sweep = warden.expire_holds(at(61)?);
        assert_eq!(sweep.dropped, 1);
        assert_eq!(sweep.thawed, 0);
        assert_eq!(sweep.cleared, 0);
        // Kein Auftauen, kein Eingriff überhaupt.
        assert!(fixture.calls.calls().is_empty());
        assert!(holds_of(&warden)?.is_empty());
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(ledger_events(&fixture)?, [LedgerEvent::HoldDropped]);
        assert!(
            fixture
                .audit
                .events()
                .iter()
                .any(|event| matches!(event, AuditEvent::HoldDropped { .. }))
        );
        assert_eq!(cleared_count(&fixture), 0);
        Ok(())
    }

    #[test]
    fn test_due_hold_of_an_already_released_containment_thaws_nothing_again() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        // Der signierte Thaw taut auf, kann den Marker aber nicht entfernen:
        // der Datensatz bleibt als freigegeben stehen.
        std::fs::set_permissions(fixture.holds(), std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("chmod 0777"))?;
        assert!(executed(&warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?
        )));
        std::fs::set_permissions(fixture.holds(), std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod 0755"))?;
        // Ein Hold derselben cgroup steht noch (etwa weil sein Aufheben nur im
        // Speicher galt und der Warden neu startete).
        warden
            .state()
            .holds()
            .arm(job_hold()?)
            .map_err(ctx("arm hold"))?;
        let sweep = warden.expire_holds(at(61)?);
        assert_eq!(sweep.dropped, 1);
        assert_eq!(sweep.thawed, 0);
        // Den freigegebenen Datensatz und seinen Marker räumt der Durchlauf ab.
        assert_eq!(sweep.cleared, 1);
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        // Genau ein Auftauen: das des signierten Thaw (inzwischen könnte harw
        // den Job angehalten haben).
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Thaw(cgroup(JOB)?)
            ]
        );
        Ok(())
    }

    #[test]
    fn test_unwritable_hold_store_takes_back_the_containment_record() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        // Jedes Schreiben des Hold-Speichers scheitert (`O_NOFOLLOW`).
        std::os::unix::fs::symlink(
            fixture.dir.path().join("elsewhere"),
            fixture.dir.path().join(format!("{HOLDS_FILE}.tmp")),
        )
        .map_err(ctx("block the hold store"))?;
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
            at(1)?,
        );
        assert_eq!(outcome, WardenOutcome::Unavailable);
        assert!(fixture.calls.calls().is_empty());
        assert!(!fixture.marker(KERNEL_ID).exists());
        // Der vorab gestellte Datensatz ist zurückgenommen, auch in der Datei.
        assert!(containments(&warden)?.is_empty());
        assert!(stored_containments(&fixture)?.is_empty());
        assert!(holds_of(&warden)?.is_empty());
        assert_eq!(
            ledger_events(&fixture)?,
            [LedgerEvent::Attempt, LedgerEvent::Failed]
        );
        assert!(matches!(
            fixture.audit.events().last(),
            Some(AuditEvent::Unavailable {
                reason: "hold store not writable; nothing executed",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn test_freeze_whose_attribution_cannot_be_persisted_freezes_nothing() -> TestResult {
        let fixture = Fixture::new()?;
        // Der Speicher wird unschreibbar, nachdem der Datensatz steht und
        // bevor `freeze_was_set` gebucht ist.
        let freezer = StoreBlockingFreezer::new(&fixture, false, true);
        let mut parts = fixture.default_parts()?;
        parts.freezer = Box::new(freezer);
        let warden = Warden::new(parts);
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(60), 1)?,
            at(1)?,
        );
        assert_eq!(outcome, WardenOutcome::Unavailable);
        assert!(matches!(
            outcome.to_reply().outcome,
            WardenResponse::Denied {
                reason: Denial::Unavailable
            }
        ));
        // Nichts eingefroren, Marker und Hold zurückgenommen.
        assert!(fixture.calls.calls().is_empty());
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(holds_of(&warden)?.is_empty());
        assert!(containments(&warden)?.is_empty());
        // In der Datei steht höchstens der nicht abgeschlossene Datensatz;
        // nach einem Absturz räumt ihn der Abgleich ab (Bit 0) oder schließt
        // ihn als fremdes Bit ab (Bit eines anderen), nie als eigenes.
        assert!(
            stored_containments(&fixture)?
                .iter()
                .all(|entry| !entry.is_complete())
        );
        assert_eq!(
            ledger_events(&fixture)?,
            [LedgerEvent::Attempt, LedgerEvent::Failed]
        );
        assert!(matches!(
            fixture.audit.events().last(),
            Some(AuditEvent::Unavailable {
                reason: "containment store not writable; nothing executed",
                ..
            })
        ));
        assert!(
            !fixture
                .audit
                .events()
                .iter()
                .any(|event| matches!(event, AuditEvent::Executed { .. }))
        );
        Ok(())
    }

    #[test]
    fn test_refreeze_whose_new_attribution_cannot_be_persisted_keeps_the_old_one() -> TestResult {
        let fixture = Fixture::new()?;
        // Erster Freeze: das Bit gehörte harw (`job.suspend`).
        let freezer = StoreBlockingFreezer::new(&fixture, true, false);
        let mut parts = fixture.default_parts()?;
        parts.freezer = Box::new(freezer.clone());
        let warden = Warden::new(parts);
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        assert!(fixture.marker(KERNEL_ID).is_file());
        // harw hat seinen Freeze aufgehoben; der zweite Freeze würde das Bit
        // selbst schreiben, kann das aber nicht dauerhaft buchen.
        freezer.set(false, true);
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?,
        );
        assert_eq!(outcome, WardenOutcome::Unavailable);
        // Nur der erste Freeze hat geschrieben; die aktive Eindämmung bleibt
        // samt Marker und mit ihrer bisherigen Zuordnung, im Speicher und in
        // der Datei.
        assert_eq!(
            fixture.calls.calls(),
            vec![RecordedCall::Freeze(cgroup(JOB)?)]
        );
        assert!(fixture.marker(KERNEL_ID).is_file());
        for records in [containments(&warden)?, stored_containments(&fixture)?] {
            assert_eq!(records.len(), 1);
            assert!(
                records
                    .iter()
                    .all(|entry| entry.freeze_was_set == Some(true) && entry.is_active())
            );
        }
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Executed,
                LedgerEvent::Attempt,
                LedgerEvent::Failed
            ]
        );
        assert!(matches!(
            fixture.audit.events().last(),
            Some(AuditEvent::Unavailable {
                reason: "containment store not writable; nothing executed",
                ..
            })
        ));
        Ok(())
    }

    // -- Runde 4: Reihenfolge der Freigabe, Rücknahme, Boot-ID, Holds ---------

    // Lässt jedes Schreiben von `file` im StateDirectory scheitern (Symlink
    // auf der Zwischendatei, `O_NOFOLLOW`).
    fn block_writes(fixture: &Fixture, file: &str) -> TestResult {
        std::os::unix::fs::symlink(
            fixture.dir.path().join("elsewhere"),
            fixture.dir.path().join(format!("{file}.tmp")),
        )
        .map_err(ctx("block writes"))
    }

    // Hebt `block_writes` wieder auf.
    fn unblock_writes(fixture: &Fixture, file: &str) -> TestResult {
        std::fs::remove_file(fixture.dir.path().join(format!("{file}.tmp")))
            .map_err(ctx("unblock writes"))
    }

    #[test]
    fn test_signed_thaw_of_a_foreign_freeze_books_the_release_before_anything_else() -> TestResult {
        let fixture = Fixture::new()?;
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        // harw hat den Job angehalten (`job.suspend`); der Warden dämmt ein.
        let inode = fixture_cgroup(root.path(), b"1\n")?;
        let control = root.path().join(JOB).join("cgroup.freeze");
        let warden = real_cgroup_warden(&fixture, root.path())?;
        assert!(executed(&warden.handle(
            &request_for(&freeze()?, EscalationStage::RuleTriggered, 1, inode)?,
            at(1)?
        )));
        // Ohne dauerhafte Buchung keine Freigabe: nichts gemeldet, nichts
        // abgeräumt, der Datensatz bleibt aktiv (im Speicher und in der
        // Datei) und mit ihm der Marker.
        block_writes(&fixture, CONTAINMENT_FILE)?;
        let refused = warden.handle(
            &request_for(&thaw()?, EscalationStage::RuleTriggered, 2, inode)?,
            at(2)?,
        );
        assert_eq!(refused, WardenOutcome::Unavailable);
        assert!(fixture.marker(inode).is_file());
        for records in [containments(&warden)?, stored_containments(&fixture)?] {
            assert_eq!(records.len(), 1);
            assert!(
                records
                    .iter()
                    .all(|entry| entry.is_active() && entry.freeze_was_set == Some(true))
            );
        }
        assert!(matches!(
            fixture.audit.events().last(),
            Some(AuditEvent::Unavailable {
                reason: "containment store not writable; nothing executed",
                ..
            })
        ));
        unblock_writes(&fixture, CONTAINMENT_FILE)?;

        // Mit dauerhafter Buchung: `executed`, auch wenn der Marker gerade
        // nicht entfernt werden kann; die Datei sagt dann schon „freigegeben“.
        std::fs::set_permissions(fixture.holds(), std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("chmod 0777"))?;
        assert!(executed(&warden.handle(
            &request_for(&thaw()?, EscalationStage::RuleTriggered, 3, inode)?,
            at(3)?
        )));
        std::fs::set_permissions(fixture.holds(), std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod 0755"))?;
        assert!(fixture.marker(inode).is_file());
        let stored = stored_containments(&fixture)?;
        assert_eq!(stored.len(), 1);
        assert!(stored.iter().all(|entry| entry.released));
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Executed,
                LedgerEvent::Attempt,
                LedgerEvent::Failed,
                LedgerEvent::Attempt,
                LedgerEvent::Executed
            ]
        );
        // Absturz jetzt: der Neustart legt den Marker nicht neu an, sondern
        // räumt ihn ab; `job.resume` gehört wieder harw.
        drop(warden);
        let restarted = real_cgroup_warden(&fixture, root.path())?;
        let report = restarted
            .reconcile_containments()
            .map_err(ctx("reconcile"))?;
        assert_eq!(report.released, 1);
        assert_eq!(report.kept, 0);
        assert_eq!(report.restored, 0);
        assert!(!fixture.marker(inode).exists());
        assert!(containments(&restarted)?.is_empty());
        // harws Freeze stand die ganze Zeit (der Freeze des Wardens schrieb
        // dasselbe Bit noch einmal).
        assert_eq!(
            std::fs::read_to_string(&control).map_err(ctx("read bit"))?,
            "1"
        );
        Ok(())
    }

    #[test]
    fn test_release_of_an_own_freeze_keeps_the_marker_until_the_release_is_durable() -> TestResult {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        // Das eigene Bit: erst auftauen, dann buchen. Die Buchung gilt nur im
        // Speicher; der Marker bleibt stehen.
        block_writes(&fixture, CONTAINMENT_FILE)?;
        assert!(executed(&warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?
        )));
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Thaw(cgroup(JOB)?)
            ]
        );
        assert!(fixture.marker(KERNEL_ID).is_file());
        let memory = containments(&warden)?;
        assert_eq!(memory.len(), 1);
        assert!(memory.iter().all(|entry| entry.released));
        let stored = stored_containments(&fixture)?;
        assert_eq!(stored.len(), 1);
        assert!(stored.iter().all(ContainmentEntry::is_active));
        assert!(fixture.audit.events().iter().any(|event| matches!(
            event,
            AuditEvent::Unavailable {
                reason: "containment store not writable; release not durable, marker kept until the hold timer writes it",
                ..
            }
        )));
        // Solange die Freigabe nicht in der Datei steht, nimmt auch der
        // Durchlauf den Marker der lebenden cgroup nicht weg.
        assert_eq!(warden.expire_holds(at(3)?).cleared, 0);
        assert!(fixture.marker(KERNEL_ID).is_file());
        // Sobald der Speicher wieder schreibbar ist, schreibt der Durchlauf
        // die Freigabe nach und räumt ab.
        unblock_writes(&fixture, CONTAINMENT_FILE)?;
        let sweep = warden.expire_holds(at(3_i64.saturating_add(CONTAINMENT_SWEEP_SECS))?);
        assert_eq!(sweep.cleared, 1);
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        assert!(stored_containments(&fixture)?.is_empty());
        assert_eq!(fixture.calls.calls().len(), 2);
        Ok(())
    }

    #[test]
    fn test_crash_after_thawing_an_own_freeze_before_a_durable_release_heals_on_restart()
    -> TestResult {
        let fixture = Fixture::new()?;
        {
            let crashed = fixture.warden()?;
            assert!(executed(&crashed.handle(
                &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
                at(1)?
            )));
            block_writes(&fixture, CONTAINMENT_FILE)?;
            assert!(executed(&crashed.handle(
                &request(&thaw()?, EscalationStage::RuleTriggered, None, 2)?,
                at(2)?
            )));
        }
        unblock_writes(&fixture, CONTAINMENT_FILE)?;
        // In der Datei ist die Eindämmung noch aktiv, das Bit aber `0` (die
        // Fixture meldet es nicht gesetzt) und kein Hold läuft: Regel 3.
        let restarted = fixture.warden()?;
        let report = restarted
            .reconcile_containments()
            .map_err(ctx("reconcile"))?;
        assert_eq!(report.thawed, 1);
        assert_eq!(report.kept, 0);
        assert_eq!(report.restored, 0);
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&restarted)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_failed_refreeze_after_a_durable_reattribution_keeps_the_foreign_bit() -> TestResult {
        let fixture = Fixture::new()?;
        // Erster Freeze: das Bit gehörte harw (`job.suspend`).
        let freezer = StoreBlockingFreezer::new(&fixture, true, false);
        let mut parts = fixture.default_parts()?;
        parts.freezer = Box::new(freezer.clone());
        let warden = Warden::new(parts);
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 1)?,
            at(1)?
        )));
        // Jemand hob harws Freeze außerhalb von harw auf; der zweite Freeze
        // bucht das Bit dauerhaft als eigenes, dann scheitert das Schreiben
        // von `cgroup.freeze` (I/O-Fehler).
        freezer.set(false, false);
        freezer.fail_freezes(true);
        let outcome = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 2)?,
            at(2)?,
        );
        assert_eq!(outcome, WardenOutcome::ExecutionFailed);
        // Die frühere Zuordnung gilt wieder, im Speicher und in der Datei;
        // der Marker der aktiven Eindämmung bleibt.
        for records in [containments(&warden)?, stored_containments(&fixture)?] {
            assert_eq!(records.len(), 1);
            assert!(
                records
                    .iter()
                    .all(|entry| entry.freeze_was_set == Some(true) && entry.is_active())
            );
        }
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(
            ledger_events(&fixture)?,
            [
                LedgerEvent::Attempt,
                LedgerEvent::Executed,
                LedgerEvent::Attempt,
                LedgerEvent::Failed
            ]
        );
        // harw hält den Job inzwischen wieder an: die Freigabe lässt das Bit
        // stehen (kein Thaw) und räumt nur die Eindämmung ab.
        assert!(executed(&warden.handle(
            &request(&thaw()?, EscalationStage::RuleTriggered, None, 3)?,
            at(3)?
        )));
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Freeze(cgroup(JOB)?)
            ]
        );
        assert!(!fixture.marker(KERNEL_ID).exists());
        assert!(containments(&warden)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_records_without_a_provable_boot_never_let_a_stale_hold_thaw() -> TestResult {
        let fixture = Fixture::new()?;
        // Vor einem Neustart des Hosts ohne lesbare Boot-ID geschrieben: ein
        // eigener Freeze und sein Hold. Jetzt trägt eine neue cgroup unter
        // demselben Pfad dieselbe Kernel-ID, und harw hat sie angehalten;
        // ein Marker dieses Namens stammt aus diesem Boot.
        let stale = serde_json::to_value(ContainmentEntry {
            target: CgroupTarget::new(cgroup(JOB)?, kernel_id(KERNEL_ID)?, at(-3600)?),
            freeze_was_set: Some(false),
            released: false,
        })
        .map_err(ctx("encode record"))?;
        let file = serde_json::json!({ "version": 1, "containments": [stale] });
        std::fs::write(
            fixture.dir.path().join(CONTAINMENT_FILE),
            serde_json::to_vec(&file).map_err(ctx("encode file"))?,
        )
        .map_err(ctx("write records"))?;
        crate::holds::HoldStore::open(fixture.dir.path())
            .map_err(ctx("hold store"))?
            .arm(job_hold()?)
            .map_err(ctx("arm hold"))?;
        std::fs::write(fixture.marker(KERNEL_ID), b"").map_err(ctx("marker"))?;

        let warden = fixture.warden()?;
        assert!(containments(&warden)?.is_empty());
        let report = warden.reconcile_containments().map_err(ctx("reconcile"))?;
        assert_eq!(report.vanished, 1);
        assert_eq!(report.kept, 0);
        assert_eq!(report.restored, 0);
        assert_eq!(report.orphaned, 1);
        // Ohne Datensatz dieses Boots taut der Hold-Timer nichts auf.
        let sweep = warden.expire_holds(at(61)?);
        assert_eq!(sweep.dropped, 1);
        assert_eq!(sweep.thawed, 0);
        assert!(fixture.calls.calls().is_empty());
        assert!(fixture.marker(KERNEL_ID).is_file());
        assert_eq!(ledger_events(&fixture)?, [LedgerEvent::HoldDropped]);
        Ok(())
    }

    #[test]
    fn test_hold_forgotten_only_in_memory_never_thaws_a_later_freeze_until_released() -> TestResult
    {
        let fixture = Fixture::new()?;
        let warden = fixture.warden()?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, Some(5), 1)?,
            at(1)?
        )));
        // Der Hold-Timer taut auf, kann den erledigten Hold aber nur im
        // Speicher entfernen; in der Datei steht er noch.
        block_writes(&fixture, HOLDS_FILE)?;
        assert_eq!(warden.expire_holds(at(6)?).thawed, 1);
        assert!(holds_of(&warden)?.is_empty());
        // Ein Freeze bis zur Aufhebung müsste diesen Hold auch in der Datei
        // aufheben; ohne Schreiben läuft er nicht.
        let refused = warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 2)?,
            at(7)?,
        );
        assert_eq!(refused, WardenOutcome::Unavailable);
        assert!(!fixture.marker(KERNEL_ID).exists());
        unblock_writes(&fixture, HOLDS_FILE)?;
        assert!(executed(&warden.handle(
            &request(&freeze()?, EscalationStage::RuleTriggered, None, 3)?,
            at(8)?
        )));
        // Nach einem Neustart kommt kein Hold zurück, der diesen Freeze
        // auftauen könnte.
        drop(warden);
        let restarted = fixture.warden()?;
        assert!(holds_of(&restarted)?.is_empty());
        assert_eq!(restarted.expire_holds(at(10_000)?), HoldSweep::default());
        assert_eq!(
            fixture.calls.calls(),
            vec![
                RecordedCall::Freeze(cgroup(JOB)?),
                RecordedCall::Thaw(cgroup(JOB)?),
                RecordedCall::Freeze(cgroup(JOB)?)
            ]
        );
        assert!(fixture.marker(KERNEL_ID).is_file());
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
