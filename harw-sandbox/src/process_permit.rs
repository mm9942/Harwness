//! Einmalige und sitzungsgebundene Freigaben für Prozessausführung.
//!
//! Ein [`ProcessPermit`] ist bewusst kein Tool-Argument und kein konfigurierbarer
//! Sandbox-Schalter. Er wird von der Runtime vor der Ausführung ausgestellt und
//! vom syscall-nahen Executor atomar verbraucht beziehungsweise geprüft. Damit
//! kann ein Modell weder einen Permit aus JSON erzeugen noch einen Permit für
//! einen anderen Befehl, Worker oder Workspace wiederverwenden.
//!
//! # Ablauf von Host-Arbeitsphasen (PL-100 Phase 4, Bedrohung B22)
//! Ein [`HostApprovalScope::SessionLease`]-Permit gilt höchstens
//! [`ProcessPermitLedger::lease_ttl`] lang ab seiner Ausstellung (Vorgabe
//! [`DEFAULT_SESSION_LEASE_TTL`] = 8 h, siehe
//! [`ProcessPermitLedger::with_lease_ttl`]). Das ersetzt die frühere
//! Nutzerentscheidung vom 2026-09-24 („kein Zeitablauf“). Ein abgelaufener
//! Permit wird bei der nächsten Prüfung entfernt und liefert
//! [`ProcessPermitError::Expired`]; der Aufrufer muss neu fragen.
//! [`ProcessPermitLedger::authorize`] verlängert nie eine Gültigkeit.
//! Permits, die unter einer laufenden Phase entstehen, stellt
//! [`ProcessPermitLedger::issue_for_lease`] aus: sie enden spätestens mit
//! dem [`LeaseWindow`] der Phase, und ein abgelaufenes Fenster ergibt
//! [`ProcessPermitError::Expired`] statt eines frischen 8-h-Permits.
//!
//! # Uhr
//! Zeitpunkte sind [`LeaseInstant`]s: eine monotone Lesung ([`Instant`])
//! plus eine Uhr, die Suspend mitzählt (`CLOCK_BOOTTIME` unter Linux/Android,
//! sonst die Wanduhr). Eine Frist gilt als erreicht, sobald **eine** der
//! beiden Uhren sie erreicht. Damit endet eine 8-h-Phase nach 8 h Wanduhrzeit,
//! auch wenn der Rechner dazwischen schlief (die monotone Uhr allein stünde
//! im Suspend still), und ein Zurückstellen der Wanduhr verlängert nichts.
//!
//! Jede zeitabhängige Methode hat eine `_at`-Variante, die „jetzt“ als
//! [`LeaseInstant`] übernimmt ([`ProcessPermitLedger::issue_after_local_approval_at`],
//! [`ProcessPermitLedger::issue_for_lease_at`],
//! [`ProcessPermitLedger::authorize_at`]). Die Varianten ohne Suffix rufen
//! sie mit [`LeaseInstant::now`] auf. Tests prüfen Grenzen (auch einen
//! simulierten Suspend über [`LeaseInstant::from_parts`]) ohne `sleep`.
//!
//! # Nebenläufigkeit
//! [`ProcessPermitLedger`] ist `Send + Sync`; der gesamte Zustand liegt hinter
//! einem einzigen [`Mutex`]. Ein vergifteter Mutex ergibt
//! [`ProcessPermitError::Poisoned`] (fail-closed).
//!
//! # Examples
//! ```rust
//! use harw_sandbox::{
//!     HostApprovalScope, ProcessEnvironment, ProcessPermitError, ProcessPermitLedger,
//!     request_for_workspace,
//! };
//! use std::path::Path;
//!
//! # fn main() -> Result<(), ProcessPermitError> {
//! let ledger = ProcessPermitLedger::default();
//! let request = request_for_workspace(
//!     "session-1",
//!     "host-process-worker@1",
//!     "echo hi",
//!     Path::new("/workspace"),
//!     ProcessEnvironment::LocalHost,
//! );
//! let id = ledger.issue_after_local_approval(
//!     request.clone(),
//!     HostApprovalScope::SessionLease,
//!     None,
//! )?;
//! assert!(ledger.authorize(id, &request).is_ok());
//! assert!(ledger.authorize(id, &request).is_ok(), "a lease permit is reusable");
//! ledger.revoke(id)?;
//! assert_eq!(
//!     ledger.authorize(id, &request),
//!     Err(ProcessPermitError::UnknownPermit)
//! );
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Vorgabe-Höchstdauer einer Host-Arbeitsphase: 8 Stunden (28 800 s).
///
/// # Description
/// Gilt für [`HostApprovalScope::SessionLease`]-Permits im
/// [`ProcessPermitLedger`] und für Sitzungs- bzw. prozessweite Freigaben in
/// [`crate::HostPermitSessionRegistry`] (PL-100 Phase 4, B22). Die Crate-Wurzel
/// exportiert ihn als `harw_sandbox::DEFAULT_SESSION_LEASE_TTL`; dieselbe
/// Dauer ist auch als [`ProcessPermitLedger::DEFAULT_LEASE_TTL`] bzw.
/// [`crate::HostPermitSessionRegistry::DEFAULT_LEASE_TTL`] erreichbar.
pub const DEFAULT_SESSION_LEASE_TTL: Duration = Duration::from_secs(28_800);

/// Ein Zeitpunkt für Ablaufprüfungen von Host-Arbeitsphasen und Permits.
///
/// # Description
/// Trägt zwei Lesungen desselben Moments: eine monotone ([`Instant`], unter
/// Linux `CLOCK_MONOTONIC`, steht im Suspend still) und eine, die Suspend
/// mitzählt (`CLOCK_BOOTTIME` unter Linux/Android, sonst die Wanduhr seit
/// der Unix-Epoche). Eine Frist ([`LeaseWindow::expires_at`]) gilt als
/// erreicht, sobald **eine** der beiden Uhren sie erreicht: Suspend beendet
/// eine Phase also pünktlich nach Wanduhrzeit, und eine zurückgestellte
/// Wanduhr verlängert nichts, weil die monotone Uhr weiter begrenzt.
///
/// Es gibt bewusst keine Ordnung (`Ord`): zwei Uhren können sich
/// widersprechen; Vergleiche laufen über [`LeaseWindow`].
///
/// # Concurrency
/// `Copy`; [`Self::now`] liest nur Uhren.
///
/// # Examples
/// ```rust
/// use harw_sandbox::HostPermitSessionRegistry;
///
/// let registry = HostPermitSessionRegistry::default();
/// registry.mark_global_approval();
/// if let Some(window) = registry.global_lease() {
///     let granted = window.granted_at();
///     assert!(window.is_active_at(granted));
///     assert!(!window.is_active_at(window.expires_at()));
///     assert_eq!(
///         granted.checked_add(window.ttl()),
///         Some(window.expires_at())
///     );
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeaseInstant {
    /// Monotone Lesung (steht im Suspend still).
    monotonic: Instant,
    /// Lesung einer Uhr, die Suspend mitzählt, als Dauer seit ihrem Nullpunkt.
    since_boot: Duration,
}

impl LeaseInstant {
    /// Liest beide Uhren jetzt.
    ///
    /// # Returns
    /// Den aktuellen Zeitpunkt; nie ein Fehler (eine unlesbare Suspend-Uhr
    /// ergibt null, dann begrenzt allein die monotone Uhr).
    #[must_use]
    pub fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            since_boot: suspend_aware_now(),
        }
    }

    /// Baut einen Zeitpunkt aus beiden Lesungen (eingespeiste Uhr, Tests).
    ///
    /// # Arguments
    /// - `monotonic`: die monotone Lesung.
    /// - `since_boot`: die Lesung der Suspend-Uhr (Dauer seit ihrem Nullpunkt).
    #[must_use]
    pub fn from_parts(monotonic: Instant, since_boot: Duration) -> Self {
        Self {
            monotonic,
            since_boot,
        }
    }

    /// Die monotone Lesung.
    #[must_use]
    pub fn monotonic(&self) -> Instant {
        self.monotonic
    }

    /// Die Lesung der Suspend-Uhr.
    #[must_use]
    pub fn since_boot(&self) -> Duration {
        self.since_boot
    }

    /// Beide Uhren um `duration` vorgerückt.
    ///
    /// # Returns
    /// `None`, wenn eine Lesung nicht darstellbar wäre.
    #[must_use]
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        Some(Self {
            monotonic: self.monotonic.checked_add(duration)?,
            since_boot: self.since_boot.checked_add(duration)?,
        })
    }

    /// Beide Uhren um `duration` zurückgesetzt.
    ///
    /// # Returns
    /// `None`, wenn eine Lesung nicht darstellbar wäre.
    #[must_use]
    pub fn checked_sub(self, duration: Duration) -> Option<Self> {
        Some(Self {
            monotonic: self.monotonic.checked_sub(duration)?,
            since_boot: self.since_boot.checked_sub(duration)?,
        })
    }

    // `true`, sobald eine der beiden Uhren `deadline` erreicht hat.
    pub(crate) fn has_reached(self, deadline: Self) -> bool {
        self.monotonic >= deadline.monotonic || self.since_boot >= deadline.since_boot
    }

    // Die frühere Frist je Uhr (komponentenweises Minimum).
    pub(crate) fn earliest(self, other: Self) -> Self {
        Self {
            monotonic: self.monotonic.min(other.monotonic),
            since_boot: self.since_boot.min(other.since_boot),
        }
    }

    // Restzeit bis `deadline`: das Minimum über beide Uhren, nie negativ.
    pub(crate) fn remaining_until(self, deadline: Self) -> Duration {
        deadline
            .monotonic
            .saturating_duration_since(self.monotonic)
            .min(deadline.since_boot.saturating_sub(self.since_boot))
    }
}

// Suspend-Uhr unter Linux/Android: `CLOCK_BOOTTIME`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn suspend_aware_now() -> Duration {
    let reading = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    let secs = u64::try_from(reading.tv_sec).unwrap_or(0);
    let nanos = u64::try_from(reading.tv_nsec).unwrap_or(0);
    Duration::from_secs(secs).saturating_add(Duration::from_nanos(nanos))
}

// Suspend-Uhr anderswo: die Wanduhr. Sie zählt Suspend mit; ein Sprung
// rückwärts verlängert nichts, weil die monotone Lesung weiter begrenzt.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn suspend_aware_now() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

/// Zeitfenster einer Host-Arbeitsphase (PL-100 Phase 4, B22).
///
/// # Description
/// `granted_at` ist der Zeitpunkt der Zustimmung, `expires_at` das Ende
/// (`granted_at + ttl` auf beiden Uhren von [`LeaseInstant`]). Ein Fenster
/// ist aktiv, solange keine der beiden Uhren `expires_at` erreicht hat;
/// `jetzt == expires_at` gilt bereits als abgelaufen (dieselbe Grenze wie im
/// [`ProcessPermitLedger`]). Nur die Registry erzeugt Fenster; ein Aufrufer
/// kann keines verlängern.
///
/// # Examples
/// ```rust
/// use harw_sandbox::HostPermitSessionRegistry;
///
/// let registry = HostPermitSessionRegistry::default();
/// registry.mark_global_approval();
/// if let Some(window) = registry.global_lease() {
///     let t0 = window.granted_at();
///     assert!(window.is_active_at(t0));
///     assert_eq!(window.remaining_at(t0), HostPermitSessionRegistry::DEFAULT_LEASE_TTL);
///     assert_eq!(window.remaining_at(window.expires_at()), std::time::Duration::ZERO);
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseWindow {
    granted_at: LeaseInstant,
    expires_at: LeaseInstant,
    ttl: Duration,
}

impl LeaseWindow {
    // Neues Fenster ab `now`; `None`, wenn `now + ttl` nicht darstellbar ist.
    pub(crate) fn starting_at(now: LeaseInstant, ttl: Duration) -> Option<Self> {
        Some(Self {
            granted_at: now,
            expires_at: now.checked_add(ttl)?,
            ttl,
        })
    }

    /// Zeitpunkt der Zustimmung, die dieses Fenster eröffnet hat.
    #[must_use]
    pub fn granted_at(&self) -> LeaseInstant {
        self.granted_at
    }

    /// Erster Zeitpunkt, zu dem das Fenster nicht mehr gilt.
    #[must_use]
    pub fn expires_at(&self) -> LeaseInstant {
        self.expires_at
    }

    /// Dauer des Fensters (`expires_at - granted_at` auf beiden Uhren).
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// `true`, solange keine der beiden Uhren von `now` `expires_at` erreicht hat.
    ///
    /// # Arguments
    /// - `now`: der Prüfzeitpunkt.
    #[must_use]
    pub fn is_active_at(&self, now: LeaseInstant) -> bool {
        !now.has_reached(self.expires_at)
    }

    /// Restlaufzeit ab `now` (Minimum über beide Uhren); `Duration::ZERO`,
    /// wenn das Fenster abgelaufen ist.
    ///
    /// # Arguments
    /// - `now`: der Bezugszeitpunkt.
    #[must_use]
    pub fn remaining_at(&self, now: LeaseInstant) -> Duration {
        now.remaining_until(self.expires_at)
    }
}

/// Umgebung, die ein genehmigter Prozess benutzen darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessEnvironment {
    /// Normale isolierte Bubblewrap-Ausführung.
    StrictSandbox,
    /// Isolierte Ausführung mit dem vorher konfigurierten Cargo-Modul.
    CargoSandbox,
    /// Isolierte Ausführung mit einem einzelnen validierten tmux-Socket.
    TmuxInspection,
    /// Lokale Host-Ausführung; nur nach UI-Approval möglich.
    LocalHost,
}

/// Lebensdauer einer durch die lokale UI bestätigten Host-Freigabe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostApprovalScope {
    /// Ein Permit darf nur genau einen Auftrag ausführen.
    SingleExecution,
    /// Mehrere Aufträge derselben lokalen Sitzung dürfen Host-Ausführung
    /// beantragen, bis der Nutzer die Host-Arbeitsphase selbst beendet
    /// (Strg+H oder `/sandbox-lease revoke`) oder die Phase abläuft
    /// (höchstens [`ProcessPermitLedger::lease_ttl`], Vorgabe 8 h; PL-100
    /// Phase 4, B22 ersetzt die Nutzerentscheidung 2026-09-24 „kein
    /// Zeitablauf“). Jeder Auftrag wird trotzdem gegen Worker, Workspace
    /// und Umgebung geprüft.
    SessionLease,
}

/// Nicht erratbare Kennung eines Runtime-ausgestellten Permits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessPermitId(u64);

/// Der unveränderliche Bindungsgegenstand einer Prozessfreigabe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPermitRequest {
    /// Lokale Parent-Sitzung, für die die UI zugestimmt hat.
    pub session: String,
    /// Exakte eingebettete Worker-Definition, nicht nur deren Organisationsrolle.
    pub worker_definition: String,
    /// Kanonischer Prozessauftrag. Für Shell-Aufträge ist dies der exakt an
    /// `/bin/sh -c` zu übergebende Text.
    pub command: String,
    /// Bereits kanonische Workspace-Wurzel.
    pub workspace: PathBuf,
    /// Gewünschte, enge Prozessumgebung.
    pub environment: ProcessEnvironment,
}

impl ProcessPermitRequest {
    /// Prüft ausschließlich die strukturellen Invarianten eines Antrags.
    pub fn validate(&self) -> Result<(), ProcessPermitError> {
        if self.session.trim().is_empty()
            || self.worker_definition.trim().is_empty()
            || self.command.trim().is_empty()
        {
            return Err(ProcessPermitError::InvalidRequest);
        }
        if !self.workspace.is_absolute() {
            return Err(ProcessPermitError::WorkspaceNotAbsolute);
        }
        Ok(())
    }
}

/// Ergebnis einer erfolgreichen Permit-Prüfung; nur der Ledger kann es erzeugen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedProcessPermit {
    id: ProcessPermitId,
    request: ProcessPermitRequest,
    scope: HostApprovalScope,
}

impl GrantedProcessPermit {
    #[must_use]
    pub fn id(&self) -> ProcessPermitId {
        self.id
    }

    #[must_use]
    pub fn request(&self) -> &ProcessPermitRequest {
        &self.request
    }

    #[must_use]
    pub fn scope(&self) -> HostApprovalScope {
        self.scope
    }
}

#[derive(Debug, Clone)]
struct StoredPermit {
    request: ProcessPermitRequest,
    scope: HostApprovalScope,
    /// `None`: gilt bis zum ausdrücklichen Widerruf ([`ProcessPermitLedger::revoke`]/
    /// [`ProcessPermitLedger::revoke_session`]); nur für
    /// `SingleExecution` ohne `ttl` möglich. `Some(t)`: verfällt, sobald eine
    /// Uhr `t` erreicht ([`LeaseInstant`]). `SessionLease`-Permits tragen
    /// immer `Some`.
    expires_at: Option<LeaseInstant>,
    consumed: bool,
}

/// Runtime-eigene, nebenläufig sichere Freigabe-Registry.
///
/// Die Registry ist der Durchsetzungspunkt für Einmaligkeit und Ablauf. Ihre
/// Ausstellungsfunktionen gehören an die lokale UI-Approval-Grenze; ein Modell
/// erhält nur eine Kennung, niemals eine Möglichkeit, einen Eintrag zu erzeugen.
#[derive(Debug)]
pub struct ProcessPermitLedger {
    state: Mutex<PermitState>,
    /// Höchstdauer eines `SessionLease`-Permits ab Ausstellung; nie null.
    lease_ttl: Duration,
}

#[derive(Debug)]
struct PermitState {
    next_id: u64,
    permits: BTreeMap<ProcessPermitId, StoredPermit>,
}

impl Default for ProcessPermitLedger {
    fn default() -> Self {
        Self::with_valid_lease_ttl(DEFAULT_SESSION_LEASE_TTL)
    }
}

impl ProcessPermitLedger {
    /// Vorgabe-Höchstdauer eines `SessionLease`-Permits: 8 h (PL-100
    /// Phase 4, B22).
    pub const DEFAULT_LEASE_TTL: Duration = DEFAULT_SESSION_LEASE_TTL;

    // Baut einen leeren Ledger; `lease_ttl` ist bereits geprüft (nie null).
    fn with_valid_lease_ttl(lease_ttl: Duration) -> Self {
        Self {
            state: Mutex::new(PermitState {
                next_id: 1,
                permits: BTreeMap::new(),
            }),
            lease_ttl,
        }
    }

    /// Erzeugt einen leeren Ledger mit eigener Höchstdauer für
    /// `SessionLease`-Permits.
    ///
    /// # Description
    /// Für die Montage, sobald eine Konfiguration die Dauer liefert, und für
    /// Tests. [`Self::default`] nutzt [`Self::DEFAULT_LEASE_TTL`].
    ///
    /// # Arguments
    /// - `lease_ttl`: Höchstdauer ab Ausstellung; muss größer als null sein.
    ///
    /// # Errors
    /// [`ProcessPermitError::ZeroTtl`], wenn `lease_ttl` null ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::{ProcessPermitError, ProcessPermitLedger};
    /// use std::time::Duration;
    ///
    /// let ledger = ProcessPermitLedger::with_lease_ttl(Duration::from_secs(3600));
    /// assert!(ledger.is_ok_and(|ledger| ledger.lease_ttl() == Duration::from_secs(3600)));
    /// assert!(matches!(
    ///     ProcessPermitLedger::with_lease_ttl(Duration::ZERO),
    ///     Err(ProcessPermitError::ZeroTtl)
    /// ));
    /// ```
    pub fn with_lease_ttl(lease_ttl: Duration) -> Result<Self, ProcessPermitError> {
        if lease_ttl.is_zero() {
            return Err(ProcessPermitError::ZeroTtl);
        }
        Ok(Self::with_valid_lease_ttl(lease_ttl))
    }

    /// Höchstdauer eines `SessionLease`-Permits ab seiner Ausstellung.
    ///
    /// # Returns
    /// Den Wert aus [`Self::with_lease_ttl`] bzw. [`Self::DEFAULT_LEASE_TTL`].
    #[must_use]
    pub fn lease_ttl(&self) -> Duration {
        self.lease_ttl
    }

    /// Stellt nach einer lokalen UI-Entscheidung eine Freigabe aus; „jetzt“
    /// ist [`LeaseInstant::now`]. Siehe [`Self::issue_after_local_approval_at`].
    ///
    /// # Errors
    /// Wie [`Self::issue_after_local_approval_at`].
    pub fn issue_after_local_approval(
        &self,
        request: ProcessPermitRequest,
        scope: HostApprovalScope,
        ttl: Option<Duration>,
    ) -> Result<ProcessPermitId, ProcessPermitError> {
        self.issue_after_local_approval_at(request, scope, ttl, LeaseInstant::now())
    }

    /// Stellt nach einer lokalen UI-Entscheidung eine Freigabe aus, mit
    /// „jetzt“ = `now`.
    ///
    /// # Description
    /// `SessionLease` ist nur für `LocalHost` gültig. Damit kann eine normale
    /// Cargo- oder tmux-Modulfreigabe nicht versehentlich zur längeren
    /// Host-Arbeitsphase werden.
    ///
    /// Gültigkeit:
    /// - `SingleExecution`: `Some(ttl)` begrenzt die Gültigkeit auf
    ///   `now + ttl` (eine Einmalfreigabe soll nicht als „stilles Ja“ liegen
    ///   bleiben); `None` gilt bis zum Verbrauch oder Widerruf.
    /// - `SessionLease`: verfällt immer, spätestens nach [`Self::lease_ttl`]
    ///   (PL-100 Phase 4, B22). `None` bedeutet [`Self::lease_ttl`] ab `now`;
    ///   `Some(ttl)` wird auf [`Self::lease_ttl`] gekappt. Das passt nur zu
    ///   einer **gerade erteilten** Zustimmung. Ein Permit, der unter einer
    ///   bereits laufenden Phase entsteht, gehört an deren Fenster: dafür gibt
    ///   es [`Self::issue_for_lease_at`], das ein abgelaufenes Fenster
    ///   ablehnt, statt still einen frischen 8-h-Permit auszustellen.
    ///
    /// # Arguments
    /// - `request`: der exakt gebundene Antrag; Eigentum geht über.
    /// - `scope`: Einmalfreigabe oder Host-Arbeitsphase.
    /// - `ttl`: gewünschte Gültigkeit ab `now`, siehe oben.
    /// - `now`: der Ausstellungszeitpunkt.
    ///
    /// # Returns
    /// Die neue, nicht erratbare [`ProcessPermitId`].
    ///
    /// # Errors
    /// - [`ProcessPermitError::InvalidRequest`] /
    ///   [`ProcessPermitError::WorkspaceNotAbsolute`]: ungültiger Antrag.
    /// - [`ProcessPermitError::ZeroTtl`]: `ttl` ist `Some(0)`.
    /// - [`ProcessPermitError::SessionLeaseRequiresHost`]: `SessionLease`
    ///   ohne `LocalHost`.
    /// - [`ProcessPermitError::TtlOverflow`]: `now + ttl` ist nicht darstellbar.
    /// - [`ProcessPermitError::IdExhausted`] / [`ProcessPermitError::Poisoned`].
    ///
    /// # Concurrency
    /// Hält die interne Sperre nur für das Eintragen.
    pub fn issue_after_local_approval_at(
        &self,
        request: ProcessPermitRequest,
        scope: HostApprovalScope,
        ttl: Option<Duration>,
        now: LeaseInstant,
    ) -> Result<ProcessPermitId, ProcessPermitError> {
        request.validate()?;
        if ttl.is_some_and(|ttl| ttl.is_zero()) {
            return Err(ProcessPermitError::ZeroTtl);
        }
        if scope == HostApprovalScope::SessionLease
            && request.environment != ProcessEnvironment::LocalHost
        {
            return Err(ProcessPermitError::SessionLeaseRequiresHost);
        }
        // Eine Host-Arbeitsphase verfällt immer und nie später als
        // `lease_ttl` nach der Ausstellung (B22); Einmalfreigaben behalten
        // ihr bisheriges Verhalten.
        let effective_ttl = match scope {
            HostApprovalScope::SingleExecution => ttl,
            HostApprovalScope::SessionLease => {
                Some(ttl.map_or(self.lease_ttl, |ttl| ttl.min(self.lease_ttl)))
            }
        };
        let expires_at = match effective_ttl {
            Some(ttl) => Some(
                now.checked_add(ttl)
                    .ok_or(ProcessPermitError::TtlOverflow)?,
            ),
            None => None,
        };
        self.insert(request, scope, expires_at)
    }

    /// Stellt einen `SessionLease`-Permit unter einer laufenden
    /// Host-Arbeitsphase aus; „jetzt“ ist [`LeaseInstant::now`]. Siehe
    /// [`Self::issue_for_lease_at`].
    ///
    /// # Errors
    /// Wie [`Self::issue_for_lease_at`].
    pub fn issue_for_lease(
        &self,
        request: ProcessPermitRequest,
        window: LeaseWindow,
    ) -> Result<ProcessPermitId, ProcessPermitError> {
        self.issue_for_lease_at(request, window, LeaseInstant::now())
    }

    /// Stellt einen `SessionLease`-Permit aus, der an das Fenster `window`
    /// einer laufenden Host-Arbeitsphase gebunden ist, mit „jetzt“ = `now`.
    ///
    /// # Description
    /// Der Permit verfällt mit dem Ende von `window`, spätestens aber
    /// [`Self::lease_ttl`] nach `now` (je Uhr das frühere Ende). Ist `window`
    /// zu `now` nicht mehr aktiv, wird nichts ausgestellt: ein abgelaufenes
    /// Fenster wird nie still zu einem frischen Permit (B22, kein
    /// fail-open über ein `None`). Das Fenster liefert
    /// `HostPermitSessionRegistry::covering_binding_at` (`LeaseBinding::window`);
    /// mit derselben Bindung wird der Permit danach über
    /// `HostPermitSessionRegistry::remember_lease_permit_at` gemerkt.
    ///
    /// # Arguments
    /// - `request`: der exakt gebundene Antrag (`LocalHost`); Eigentum geht über.
    /// - `window`: das Fenster der Phase, die die Sitzung abdeckt.
    /// - `now`: der Ausstellungszeitpunkt.
    ///
    /// # Returns
    /// Die neue [`ProcessPermitId`].
    ///
    /// # Errors
    /// - [`ProcessPermitError::Expired`]: `window` ist zu `now` abgelaufen.
    /// - [`ProcessPermitError::InvalidRequest`] /
    ///   [`ProcessPermitError::WorkspaceNotAbsolute`]: ungültiger Antrag.
    /// - [`ProcessPermitError::SessionLeaseRequiresHost`]: nicht `LocalHost`.
    /// - [`ProcessPermitError::TtlOverflow`]: `now + lease_ttl` ist nicht darstellbar.
    /// - [`ProcessPermitError::IdExhausted`] / [`ProcessPermitError::Poisoned`].
    ///
    /// # Concurrency
    /// Hält die interne Sperre nur für das Eintragen.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::{
    ///     HostPermitSessionRegistry, ProcessEnvironment, ProcessPermitError,
    ///     ProcessPermitLedger, request_for_workspace,
    /// };
    /// use std::path::Path;
    ///
    /// # fn main() -> Result<(), ProcessPermitError> {
    /// let ledger = ProcessPermitLedger::default();
    /// let registry = HostPermitSessionRegistry::default();
    /// let request = request_for_workspace(
    ///     "session-1",
    ///     "host-process-worker@1",
    ///     "echo hi",
    ///     Path::new("/workspace"),
    ///     ProcessEnvironment::LocalHost,
    /// );
    /// registry.mark_session_approved("session-1");
    /// let Some(window) = registry.covering_lease("session-1") else {
    ///     return Ok(());
    /// };
    /// let now = window.granted_at();
    /// let id = ledger.issue_for_lease_at(request.clone(), window, now)?;
    /// assert!(ledger.authorize_at(id, &request, now).is_ok());
    /// // Der Permit endet mit der Phase.
    /// assert_eq!(
    ///     ledger.authorize_at(id, &request, window.expires_at()),
    ///     Err(ProcessPermitError::Expired)
    /// );
    /// // Ein abgelaufenes Fenster ergibt keinen neuen Permit.
    /// assert_eq!(
    ///     ledger.issue_for_lease_at(request, window, window.expires_at()),
    ///     Err(ProcessPermitError::Expired)
    /// );
    /// # Ok(())
    /// # }
    /// ```
    pub fn issue_for_lease_at(
        &self,
        request: ProcessPermitRequest,
        window: LeaseWindow,
        now: LeaseInstant,
    ) -> Result<ProcessPermitId, ProcessPermitError> {
        request.validate()?;
        if request.environment != ProcessEnvironment::LocalHost {
            return Err(ProcessPermitError::SessionLeaseRequiresHost);
        }
        if !window.is_active_at(now) {
            return Err(ProcessPermitError::Expired);
        }
        let cap = now
            .checked_add(self.lease_ttl)
            .ok_or(ProcessPermitError::TtlOverflow)?;
        let expires_at = cap.earliest(window.expires_at());
        self.insert(request, HostApprovalScope::SessionLease, Some(expires_at))
    }

    // Trägt einen bereits geprüften Permit unter der Sperre ein.
    fn insert(
        &self,
        request: ProcessPermitRequest,
        scope: HostApprovalScope,
        expires_at: Option<LeaseInstant>,
    ) -> Result<ProcessPermitId, ProcessPermitError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ProcessPermitError::Poisoned)?;
        let id = ProcessPermitId(state.next_id);
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or(ProcessPermitError::IdExhausted)?;
        state.permits.insert(
            id,
            StoredPermit {
                request,
                scope,
                expires_at,
                consumed: false,
            },
        );
        Ok(id)
    }

    /// Prüft die vollständige Bindung und verbraucht Einmal-Permits atomar;
    /// „jetzt“ ist [`LeaseInstant::now`]. Siehe [`Self::authorize_at`].
    ///
    /// # Errors
    /// Wie [`Self::authorize_at`].
    pub fn authorize(
        &self,
        id: ProcessPermitId,
        actual: &ProcessPermitRequest,
    ) -> Result<GrantedProcessPermit, ProcessPermitError> {
        self.authorize_at(id, actual, LeaseInstant::now())
    }

    /// Prüft die vollständige Bindung und verbraucht Einmal-Permits atomar,
    /// mit „jetzt“ = `now`.
    ///
    /// # Description
    /// Bei `SessionLease` bleibt der Eintrag bis zum Widerruf oder Ablauf
    /// erhalten, aber nur ein exakt gleicher Auftrag darf ihn verwenden. Das
    /// verhindert, dass eine Zustimmung zu `tmux capture-pane` als allgemeines
    /// Host-Shell-Recht dient. Ein Permit, dessen `expires_at` eine Uhr von
    /// `now` erreicht hat, wird entfernt. Eine erfolgreiche Prüfung verlängert
    /// die Gültigkeit nie.
    ///
    /// # Arguments
    /// - `id`: die bei der Ausstellung gelieferte Kennung.
    /// - `actual`: der tatsächlich auszuführende Antrag.
    /// - `now`: der Prüfzeitpunkt.
    ///
    /// # Returns
    /// Das [`GrantedProcessPermit`] für genau diesen Antrag.
    ///
    /// # Errors
    /// - [`ProcessPermitError::InvalidRequest`] /
    ///   [`ProcessPermitError::WorkspaceNotAbsolute`]: ungültiger Antrag.
    /// - [`ProcessPermitError::UnknownPermit`]: unbekannt oder widerrufen.
    /// - [`ProcessPermitError::Expired`]: abgelaufen (Eintrag entfernt).
    /// - [`ProcessPermitError::BindingMismatch`]: anderer Antrag.
    /// - [`ProcessPermitError::AlreadyConsumed`]: Einmal-Permit verbraucht.
    /// - [`ProcessPermitError::Poisoned`]: vergiftete Sperre.
    ///
    /// # Concurrency
    /// Prüfen und Verbrauchen laufen unter derselben Sperre; zwei
    /// gleichzeitige Prüfungen eines Einmal-Permits sind nie beide erfolgreich.
    pub fn authorize_at(
        &self,
        id: ProcessPermitId,
        actual: &ProcessPermitRequest,
        now: LeaseInstant,
    ) -> Result<GrantedProcessPermit, ProcessPermitError> {
        actual.validate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ProcessPermitError::Poisoned)?;
        let expired = state
            .permits
            .get(&id)
            .ok_or(ProcessPermitError::UnknownPermit)?
            .expires_at
            .is_some_and(|expires_at| now.has_reached(expires_at));
        if expired {
            state.permits.remove(&id);
            return Err(ProcessPermitError::Expired);
        }
        // Die Sperre ist seit der Existenzprüfung oben ununterbrochen gehalten,
        // daher kann der Eintrag hier nicht verschwunden sein; `UnknownPermit`
        // ist trotzdem der korrekte Fehler statt einer Panik (Bible R087).
        let permit = state
            .permits
            .get_mut(&id)
            .ok_or(ProcessPermitError::UnknownPermit)?;
        if permit.request != *actual {
            return Err(ProcessPermitError::BindingMismatch);
        }
        if permit.scope == HostApprovalScope::SingleExecution {
            if permit.consumed {
                return Err(ProcessPermitError::AlreadyConsumed);
            }
            permit.consumed = true;
        }
        Ok(GrantedProcessPermit {
            id,
            request: permit.request.clone(),
            scope: permit.scope,
        })
    }

    /// Widerruft einen Permit oder Lease sofort, etwa bei UI-„Isolation wieder
    /// aktivieren“, Sitzungsende oder Disconnect. Unbekannte Kennungen sind
    /// absichtlich idempotent.
    pub fn revoke(&self, id: ProcessPermitId) -> Result<(), ProcessPermitError> {
        self.state
            .lock()
            .map_err(|_| ProcessPermitError::Poisoned)?
            .permits
            .remove(&id);
        Ok(())
    }

    /// Widerruft sämtliche Permits einer Sitzung. Der Stringvergleich ist
    /// absichtlich exakt; keine Präfix- oder Teiltreffer dürfen fremde Sessions
    /// beeinflussen.
    pub fn revoke_session(&self, session: &str) -> Result<usize, ProcessPermitError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ProcessPermitError::Poisoned)?;
        let before = state.permits.len();
        state
            .permits
            .retain(|_, permit| permit.request.session != session);
        Ok(before - state.permits.len())
    }
}

/// Fail-closed Fehler der Permit-Grenze. Details über andere Permits werden nie
/// offengelegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPermitError {
    InvalidRequest,
    WorkspaceNotAbsolute,
    ZeroTtl,
    TtlOverflow,
    SessionLeaseRequiresHost,
    UnknownPermit,
    Expired,
    BindingMismatch,
    AlreadyConsumed,
    IdExhausted,
    Poisoned,
}

impl std::fmt::Display for ProcessPermitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidRequest | Self::WorkspaceNotAbsolute => "invalid process permit request",
            Self::ZeroTtl | Self::TtlOverflow => "invalid process permit lifetime",
            Self::SessionLeaseRequiresHost => "a session lease requires local host execution",
            Self::UnknownPermit | Self::Expired | Self::BindingMismatch | Self::AlreadyConsumed => {
                "process execution is not authorized"
            }
            Self::IdExhausted | Self::Poisoned => "process permit service is unavailable",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ProcessPermitError {}

/// Helper for callers that already hold a canonical workspace root.
#[must_use]
pub fn request_for_workspace(
    session: impl Into<String>,
    worker_definition: impl Into<String>,
    command: impl Into<String>,
    workspace: &Path,
    environment: ProcessEnvironment,
) -> ProcessPermitRequest {
    ProcessPermitRequest {
        session: session.into(),
        worker_definition: worker_definition.into(),
        command: command.into(),
        workspace: workspace.to_path_buf(),
        environment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn request(environment: ProcessEnvironment) -> ProcessPermitRequest {
        request_for_workspace(
            "local-session",
            "host-process-worker@1",
            "tmux ls",
            Path::new("/workspace"),
            environment,
        )
    }

    #[test]
    fn single_execution_is_bound_and_consumed() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let request = request(ProcessEnvironment::LocalHost);
        let id = ledger
            .issue_after_local_approval(
                request.clone(),
                HostApprovalScope::SingleExecution,
                Some(Duration::from_secs(30)),
            )
            .map_err(ctx("issue_after_local_approval failed"))?;
        assert!(ledger.authorize(id, &request).is_ok());
        assert_eq!(
            ledger.authorize(id, &request),
            Err(ProcessPermitError::AlreadyConsumed)
        );

        let mut changed = request;
        changed.command = "tmux kill-server".to_owned();
        assert_eq!(
            ledger.authorize(id, &changed),
            Err(ProcessPermitError::BindingMismatch)
        );
        Ok(())
    }

    #[test]
    fn session_lease_is_host_only_and_revocable() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let strict = request(ProcessEnvironment::StrictSandbox);
        assert_eq!(
            ledger.issue_after_local_approval(strict, HostApprovalScope::SessionLease, None),
            Err(ProcessPermitError::SessionLeaseRequiresHost)
        );
        let host = request(ProcessEnvironment::LocalHost);
        let id = ledger
            .issue_after_local_approval(host.clone(), HostApprovalScope::SessionLease, None)
            .map_err(ctx("issue_after_local_approval failed"))?;
        assert!(ledger.authorize(id, &host).is_ok());
        assert!(ledger.authorize(id, &host).is_ok());
        assert_eq!(ledger.revoke_session("local-session"), Ok(1));
        assert_eq!(
            ledger.authorize(id, &host),
            Err(ProcessPermitError::UnknownPermit)
        );
        Ok(())
    }

    // Dauer in Stunden bzw. Minuten, gesättigt statt überlaufend.
    fn hours(count: u64) -> Duration {
        Duration::from_secs(count.saturating_mul(60 * 60))
    }

    fn minutes(count: u64) -> Duration {
        Duration::from_secs(count.saturating_mul(60))
    }

    // Liefert `base + offset` oder einen Testfehler statt einer Panik.
    fn later(base: LeaseInstant, offset: Duration) -> TestResult<LeaseInstant> {
        base.checked_add(offset)
            .ok_or(TestError::Missing("instant after offset"))
    }

    // Liefert `instant - 1 ns`: der letzte Zeitpunkt vor einer Grenze.
    fn just_before(instant: LeaseInstant) -> TestResult<LeaseInstant> {
        instant
            .checked_sub(Duration::from_nanos(1))
            .ok_or(TestError::Missing("instant one nanosecond earlier"))
    }

    #[test]
    fn default_lease_ttl_is_eight_hours_and_zero_is_rejected() {
        assert_eq!(ProcessPermitLedger::DEFAULT_LEASE_TTL, hours(8));
        assert_eq!(
            ProcessPermitLedger::default().lease_ttl(),
            ProcessPermitLedger::DEFAULT_LEASE_TTL
        );
        assert!(matches!(
            ProcessPermitLedger::with_lease_ttl(Duration::ZERO),
            Err(ProcessPermitError::ZeroTtl)
        ));
        assert!(
            ProcessPermitLedger::with_lease_ttl(Duration::from_secs(60))
                .is_ok_and(|ledger| ledger.lease_ttl() == Duration::from_secs(60))
        );
    }

    #[test]
    fn session_lease_without_ttl_expires_at_default_lease_boundary() -> TestResult {
        // PL-100 Phase 4 (B22): ersetzt den früheren Test
        // `session_lease_without_ttl_does_not_expire` (Nutzerentscheidung
        // 2026-09-24, kein Zeitablauf).
        let ledger = ProcessPermitLedger::default();
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();
        let id = ledger
            .issue_after_local_approval_at(host.clone(), HostApprovalScope::SessionLease, None, t0)
            .map_err(ctx("issue_after_local_approval_at failed"))?;
        let boundary = later(t0, ProcessPermitLedger::DEFAULT_LEASE_TTL)?;

        // Wiederholte Prüfungen innerhalb der Phase verlängern nichts.
        assert!(ledger.authorize_at(id, &host, t0).is_ok());
        assert!(ledger.authorize_at(id, &host, later(t0, hours(7))?).is_ok());
        assert!(
            ledger
                .authorize_at(id, &host, just_before(boundary)?)
                .is_ok()
        );
        assert_eq!(
            ledger.authorize_at(id, &host, boundary),
            Err(ProcessPermitError::Expired),
            "now == expires_at must count as expired"
        );
        // Der abgelaufene Eintrag ist entfernt; eine spätere Prüfung kennt
        // ihn nicht mehr und muss neu ausgestellt werden.
        assert_eq!(
            ledger.authorize_at(id, &host, boundary),
            Err(ProcessPermitError::UnknownPermit)
        );
        Ok(())
    }

    #[test]
    fn session_lease_ttl_is_capped_at_lease_ttl_and_shorter_ttl_is_kept() -> TestResult {
        let lease_ttl = hours(1);
        let ledger =
            ProcessPermitLedger::with_lease_ttl(lease_ttl).map_err(ctx("with_lease_ttl failed"))?;
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();

        let capped = ledger
            .issue_after_local_approval_at(
                host.clone(),
                HostApprovalScope::SessionLease,
                Some(hours(24)),
                t0,
            )
            .map_err(ctx("issue with a longer ttl failed"))?;
        assert!(
            ledger
                .authorize_at(capped, &host, just_before(later(t0, lease_ttl)?)?)
                .is_ok()
        );
        assert_eq!(
            ledger.authorize_at(capped, &host, later(t0, lease_ttl)?),
            Err(ProcessPermitError::Expired),
            "a session lease must never outlive lease_ttl"
        );

        let shorter = minutes(10);
        let short = ledger
            .issue_after_local_approval_at(
                host.clone(),
                HostApprovalScope::SessionLease,
                Some(shorter),
                t0,
            )
            .map_err(ctx("issue with a shorter ttl failed"))?;
        assert_eq!(
            ledger.authorize_at(short, &host, later(t0, shorter)?),
            Err(ProcessPermitError::Expired),
            "a shorter ttl (remaining lease time) must be honoured"
        );
        Ok(())
    }

    #[test]
    fn single_execution_lifetime_is_not_tied_to_lease_ttl() -> TestResult {
        // Regression: die Einmalfreigabe behält ihre eigene TTL (Shell:
        // 5 min) bzw. ohne TTL gar keinen Ablauf.
        let ledger = ProcessPermitLedger::with_lease_ttl(minutes(1))
            .map_err(ctx("with_lease_ttl failed"))?;
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();
        let five_minutes = minutes(5);
        let id = ledger
            .issue_after_local_approval_at(
                host.clone(),
                HostApprovalScope::SingleExecution,
                Some(five_minutes),
                t0,
            )
            .map_err(ctx("issue single execution failed"))?;
        assert!(
            ledger
                .authorize_at(id, &host, just_before(later(t0, five_minutes)?)?)
                .is_ok()
        );

        let unbounded = ledger
            .issue_after_local_approval_at(
                host.clone(),
                HostApprovalScope::SingleExecution,
                None,
                t0,
            )
            .map_err(ctx("issue unbounded single execution failed"))?;
        assert!(
            ledger
                .authorize_at(unbounded, &host, later(t0, hours(100))?)
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn lifetime_overflow_fails_closed() -> TestResult {
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();
        let ledger = ProcessPermitLedger::default();
        assert_eq!(
            ledger.issue_after_local_approval_at(
                host.clone(),
                HostApprovalScope::SingleExecution,
                Some(Duration::MAX),
                t0,
            ),
            Err(ProcessPermitError::TtlOverflow)
        );
        // Bei `SessionLease` wird `Duration::MAX` auf `lease_ttl` gekappt.
        assert!(
            ledger
                .issue_after_local_approval_at(
                    host.clone(),
                    HostApprovalScope::SessionLease,
                    Some(Duration::MAX),
                    t0,
                )
                .is_ok()
        );
        let unbounded = ProcessPermitLedger::with_lease_ttl(Duration::MAX)
            .map_err(ctx("with_lease_ttl failed"))?;
        assert_eq!(
            unbounded.issue_after_local_approval_at(
                host,
                HostApprovalScope::SessionLease,
                None,
                t0
            ),
            Err(ProcessPermitError::TtlOverflow)
        );
        Ok(())
    }

    #[test]
    fn permit_with_ttl_still_expires() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let host = request(ProcessEnvironment::LocalHost);
        let id = ledger
            .issue_after_local_approval(
                host.clone(),
                HostApprovalScope::SingleExecution,
                Some(Duration::from_millis(10)),
            )
            .map_err(ctx("issue_after_local_approval failed"))?;
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(
            ledger.authorize(id, &host),
            Err(ProcessPermitError::Expired)
        );
        Ok(())
    }

    // `base`, bei dem nur die Suspend-Uhr um `slept` weitergelaufen ist:
    // so sieht „jetzt“ nach einem Suspend aus (die monotone Uhr stand still).
    fn after_suspend(base: LeaseInstant, slept: Duration) -> TestResult<LeaseInstant> {
        let since_boot = base
            .since_boot()
            .checked_add(slept)
            .ok_or(TestError::Missing("boot time after suspend"))?;
        Ok(LeaseInstant::from_parts(base.monotonic(), since_boot))
    }

    #[test]
    fn suspend_counts_toward_session_lease_expiry() -> TestResult {
        // B22: 8 h Phase, davon 7 h im Suspend. Monoton sind erst 2 h
        // vergangen, nach Wanduhr 9 h: der Permit ist abgelaufen.
        let ledger = ProcessPermitLedger::default();
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();
        let id = ledger
            .issue_after_local_approval_at(host.clone(), HostApprovalScope::SessionLease, None, t0)
            .map_err(ctx("issue_after_local_approval_at failed"))?;
        let awake_two_hours = later(t0, hours(2))?;
        assert!(ledger.authorize_at(id, &host, awake_two_hours).is_ok());
        let resumed = after_suspend(awake_two_hours, hours(7))?;
        assert_eq!(
            ledger.authorize_at(id, &host, resumed),
            Err(ProcessPermitError::Expired),
            "suspend time must count toward the lease"
        );
        Ok(())
    }

    #[test]
    fn suspend_clock_alone_reaching_the_deadline_expires_exactly_at_the_boundary() -> TestResult {
        let ledger =
            ProcessPermitLedger::with_lease_ttl(hours(1)).map_err(ctx("with_lease_ttl failed"))?;
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();
        let id = ledger
            .issue_after_local_approval_at(host.clone(), HostApprovalScope::SessionLease, None, t0)
            .map_err(ctx("issue failed"))?;
        // Monoton 1 min, Suspend-Uhr bis 1 ns vor der Grenze: noch gültig.
        let almost = after_suspend(
            later(t0, minutes(1))?,
            hours(1)
                .checked_sub(minutes(1))
                .and_then(|rest| rest.checked_sub(Duration::from_nanos(1)))
                .ok_or(TestError::Missing("rest"))?,
        )?;
        assert!(ledger.authorize_at(id, &host, almost).is_ok());
        // Genau auf der Grenze der Suspend-Uhr: abgelaufen.
        let exactly = after_suspend(later(t0, minutes(1))?, minutes(59))?;
        assert_eq!(
            ledger.authorize_at(id, &host, exactly),
            Err(ProcessPermitError::Expired)
        );
        Ok(())
    }

    #[test]
    fn a_wall_clock_set_back_does_not_extend_the_lease() -> TestResult {
        // Die Suspend-Uhr steht (oder läuft rückwärts, Wanduhr-Rückfall): die
        // monotone Uhr begrenzt weiter.
        let ledger = ProcessPermitLedger::default();
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();
        let id = ledger
            .issue_after_local_approval_at(host.clone(), HostApprovalScope::SessionLease, None, t0)
            .map_err(ctx("issue failed"))?;
        let monotonic_end = later(t0, ProcessPermitLedger::DEFAULT_LEASE_TTL)?.monotonic();
        let set_back = LeaseInstant::from_parts(monotonic_end, Duration::ZERO);
        assert_eq!(
            ledger.authorize_at(id, &host, set_back),
            Err(ProcessPermitError::Expired)
        );
        Ok(())
    }

    #[test]
    fn issue_for_lease_binds_the_permit_to_the_window() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();
        let window = LeaseWindow::starting_at(t0, hours(8)).ok_or(TestError::Missing("window"))?;

        // Spät in der Phase ausgestellt: endet mit der Phase, nicht 8 h später.
        let t7 = later(t0, hours(7))?;
        let id = ledger
            .issue_for_lease_at(host.clone(), window, t7)
            .map_err(ctx("issue_for_lease_at failed"))?;
        assert!(
            ledger
                .authorize_at(id, &host, just_before(window.expires_at())?)
                .is_ok()
        );
        assert_eq!(
            ledger.authorize_at(id, &host, window.expires_at()),
            Err(ProcessPermitError::Expired)
        );

        // Abgelaufenes Fenster: kein still ausgestellter frischer Permit.
        assert_eq!(
            ledger.issue_for_lease_at(host.clone(), window, window.expires_at()),
            Err(ProcessPermitError::Expired)
        );
        let slept = after_suspend(t7, hours(2))?;
        assert_eq!(
            ledger.issue_for_lease_at(host.clone(), window, slept),
            Err(ProcessPermitError::Expired)
        );

        // Nur `LocalHost`.
        assert_eq!(
            ledger.issue_for_lease_at(request(ProcessEnvironment::StrictSandbox), window, t7),
            Err(ProcessPermitError::SessionLeaseRequiresHost)
        );
        Ok(())
    }

    #[test]
    fn issue_for_lease_is_capped_at_the_ledger_lease_ttl() -> TestResult {
        let ledger =
            ProcessPermitLedger::with_lease_ttl(hours(1)).map_err(ctx("with_lease_ttl failed"))?;
        let host = request(ProcessEnvironment::LocalHost);
        let t0 = LeaseInstant::now();
        let long_window =
            LeaseWindow::starting_at(t0, hours(24)).ok_or(TestError::Missing("window"))?;
        let id = ledger
            .issue_for_lease_at(host.clone(), long_window, t0)
            .map_err(ctx("issue_for_lease_at failed"))?;
        assert!(
            ledger
                .authorize_at(id, &host, just_before(later(t0, hours(1))?)?)
                .is_ok()
        );
        assert_eq!(
            ledger.authorize_at(id, &host, later(t0, hours(1))?),
            Err(ProcessPermitError::Expired)
        );
        Ok(())
    }

    #[test]
    fn lease_window_uses_the_earlier_clock() -> TestResult {
        let t0 = LeaseInstant::now();
        let window = LeaseWindow::starting_at(t0, hours(8)).ok_or(TestError::Missing("window"))?;
        assert_eq!(window.granted_at(), t0);
        assert_eq!(window.ttl(), hours(8));
        assert_eq!(window.expires_at(), later(t0, hours(8))?);
        assert_eq!(window.remaining_at(t0), hours(8));
        // Nach 1 h wach und 3 h Suspend bleiben 4 h, nicht 7 h.
        let resumed = after_suspend(later(t0, hours(1))?, hours(3))?;
        assert!(window.is_active_at(resumed));
        assert_eq!(window.remaining_at(resumed), hours(4));
        assert!(!window.is_active_at(after_suspend(t0, hours(8))?));
        assert_eq!(
            window.remaining_at(after_suspend(t0, hours(9))?),
            Duration::ZERO
        );
        assert!(LeaseWindow::starting_at(t0, Duration::MAX).is_none());
        Ok(())
    }

    #[test]
    fn lease_instant_now_reads_a_suspend_aware_clock() {
        let first = LeaseInstant::now();
        let second = LeaseInstant::now();
        assert!(second.monotonic() >= first.monotonic());
        assert!(second.since_boot() >= first.since_boot());
        #[cfg(any(target_os = "linux", target_os = "android"))]
        assert!(
            first.since_boot() > Duration::ZERO,
            "CLOCK_BOOTTIME must be readable"
        );
    }
}
