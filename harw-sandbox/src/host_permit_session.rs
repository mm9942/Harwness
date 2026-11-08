//! Sitzungsseitige Zuordnung von UI-Bestätigungen zu [`ProcessPermitId`]s.
//!
//! [`ProcessPermitLedger`] (siehe `process_permit.rs`) bindet einen Permit an
//! genau einen [`ProcessPermitRequest`] und weiß nichts über "Sitzungen" im
//! Sinn einer laufenden TUI-Konversation. Dieses Modul ergänzt genau die
//! Anbindung, die zwischen einer einmaligen lokalen UI-Zustimmung ("Host-
//! Zugriff für diese Sitzung erlauben") und den vielen, textuell
//! unterschiedlichen Einzelbefehlen liegt, die ein Host-Profil-Worker danach
//! stellt: die Registry merkt sich, dass eine Sitzung *grundsätzlich*
//! zugestimmt hat, und merkt sich zusätzlich pro bereits ausgestelltem
//! Permit, welcher exakte Antrag ihn trägt — nur dieser exakte Antrag darf
//! ihn nach [`ProcessPermitLedger::authorize`] wiederverwenden.
//!
//! # Prozessweite Freigabe (Nutzerwunsch „volle Sandbox-Deaktivierung“,
//! 2026-09-21)
//! Neben der sitzungseigenen Zustimmung (`mark_session_approved`/
//! `mark_single_use`, oben) kennt die Registry zusätzlich eine *globale*
//! Freigabe (`mark_global_approval`/`mark_global_single_use`), die für
//! **jede** Session-ID desselben `harw`-Prozesses gilt — Root-Session und
//! jede Kind-Session, die dieselbe `Arc<HostPermitSessionRegistry>`-Instanz
//! teilt. `/sandbox-lease` (siehe `harw-ops/src/sandbox_lease.rs`) setzt
//! ausschließlich die globale Freigabe; die rein sitzungseigenen Methoden
//! bleiben für Aufrufer erhalten, die bewusst nur eine einzelne Sitzung
//! freigeben wollen. `is_session_approved`, `take_single_use` und
//! `has_single_use` berücksichtigen beide Zustände gemeinsam (siehe die
//! jeweilige Methodendoku).
//!
//! # Ablauf (PL-100 Phase 4, Bedrohung B22)
//! Eine Sitzungs- oder prozessweite Freigabe (Host-Arbeitsphase) trägt ein
//! [`LeaseWindow`] mit `granted_at` und `expires_at` (je ein
//! [`LeaseInstant`]: monotone Uhr plus `CLOCK_BOOTTIME`, das Suspend
//! mitzählt; abgelaufen, sobald eine der beiden Uhren das Ende erreicht).
//! Die Dauer ist
//! [`HostPermitSessionRegistry::lease_ttl`] (Vorgabe
//! [`HostPermitSessionRegistry::DEFAULT_LEASE_TTL`] = 8 h, einstellbar über
//! [`HostPermitSessionRegistry::with_lease_ttl`]). Das ersetzt die frühere
//! Nutzerentscheidung vom 2026-09-24 („kein Zeitablauf“).
//! - Eine abgelaufene Phase (eine Uhr hat `expires_at` erreicht) gilt als nicht vorhanden:
//!   [`HostPermitSessionRegistry::is_session_approved`] und
//!   [`HostPermitSessionRegistry::has_global_approval`] liefern `false`, der
//!   Aufrufer fragt erneut.
//! - Eine Phase wird nie stillschweigend verlängert. Ein erneutes Markieren
//!   einer noch aktiven Phase ändert nichts; erst nach dem Ablauf (oder nach
//!   einem Widerruf) beginnt mit einer neuen Zustimmung ein neues Fenster.
//! - Ein gemerkter Permit, der unter einer Phase entstand, wird nach deren
//!   Ende von [`HostPermitSessionRegistry::lookup_permit`] nicht mehr
//!   geliefert. Neue Permits unter einer Phase stellt
//!   [`crate::ProcessPermitLedger::issue_for_lease`] mit dem Fenster aus
//!   [`HostPermitSessionRegistry::covering_lease`] aus; sie enden mit der
//!   Phase.
//! - Der Nutzer kann eine Phase weiterhin jederzeit selbst beenden: Strg+H in
//!   der TUI (`ChatApp::end_host_mode`) oder das getippte
//!   `/sandbox-lease revoke`. Das Modell kann eine Phase weder beenden noch
//!   verlängern. Einmal-Freigaben bleiben einmalig.
//! - Abgelaufene Fenster bleiben bis zum Widerruf oder zur nächsten Zustimmung
//!   gespeichert ([`HostPermitSessionRegistry::session_lease`],
//!   [`HostPermitSessionRegistry::global_lease`]), damit eine Oberfläche
//!   „abgelaufen“ anzeigen kann. Für die Autorisierung sind sie wirkungslos.
//!
//! Der Zustand ist rein In-Memory; ein Prozessende beendet jede Phase ohnehin.
//!
//! # Uhr
//! Jede zeitabhängige Methode hat eine `_at`-Variante, die „jetzt“ als
//! [`LeaseInstant`] übernimmt. Die Varianten ohne Suffix rufen sie mit
//! [`LeaseInstant::now`] auf. Ein Suspend lässt sich in Tests über
//! [`LeaseInstant::from_parts`] nachstellen (nur die Suspend-Uhr läuft).
//!
//! # Verantwortungsgrenze
//! Diese Datei ändert nichts an `process_permit.rs`: sie hält ausschließlich
//! Zusatzzustand für den Aufrufer (den Shell-Executor und die lokale
//! Bestätigungs-UI), damit beide denselben "hat diese Sitzung schon
//! zugestimmt"-Zustand sehen, ohne die Ledger-Kernlogik zu erweitern.
//!
//! # Nebenläufigkeit
//! [`HostPermitSessionRegistry`] ist `Send + Sync`; der gesamte Zustand liegt
//! hinter einem einzigen [`Mutex`].
//!
//! # Examples
//! ```rust
//! use harw_sandbox::HostPermitSessionRegistry;
//!
//! let registry = HostPermitSessionRegistry::default();
//! registry.mark_session_approved("session-1");
//! if let Some(window) = registry.session_lease("session-1") {
//!     assert!(registry.is_session_approved_at("session-1", window.granted_at()));
//!     // Abgelaufen: gilt als nicht vorhanden, der Aufrufer fragt erneut.
//!     assert!(!registry.is_session_approved_at("session-1", window.expires_at()));
//! }
//! ```

use crate::process_permit::{
    DEFAULT_SESSION_LEASE_TTL, LeaseInstant, LeaseWindow, ProcessPermitError, ProcessPermitId,
    ProcessPermitRequest,
};
use std::sync::Mutex;
use std::time::Duration;

/// Ein Eintrag: welcher exakte Antrag bereits einen ausgestellten Permit hat.
#[derive(Debug, Clone)]
struct RememberedPermit {
    request: ProcessPermitRequest,
    id: ProcessPermitId,
    /// `true`, wenn beim Merken eine aktive Host-Arbeitsphase die Sitzung des
    /// Antrags abdeckte. Dann liefert `lookup_permit` den Eintrag nur, solange
    /// eine Phase die Sitzung weiterhin abdeckt (B22: kein Weiterleben nach
    /// dem Ablauf).
    lease_bound: bool,
}

/// Eine sitzungseigene Host-Arbeitsphase.
#[derive(Debug, Clone)]
struct ApprovedSession {
    session: String,
    window: LeaseWindow,
}

#[derive(Debug, Default)]
struct RegistryState {
    /// Sitzungen, deren Nutzer der lokalen UI für Host-Ausführung zugestimmt
    /// hat, je mit ihrem [`LeaseWindow`]. Ein abgelaufenes Fenster bleibt bis
    /// zum Widerruf ([`HostPermitSessionRegistry::forget_session`]/
    /// [`HostPermitSessionRegistry::revoke_session_approval`]) oder zur
    /// nächsten Zustimmung gespeichert, ist aber wirkungslos.
    approved_sessions: Vec<ApprovedSession>,
    /// Bereits ausgestellte Permits, um denselben Antrag ohne erneute
    /// `issue_after_local_approval`-Ausstellung wiederzuverwenden.
    remembered: Vec<RememberedPermit>,
    /// Sitzungen mit einer noch nicht verbrauchten Einmal-Freigabe
    /// (`HostApprovalScope::SingleExecution`-Zustimmung im
    /// Bestätigungsdialog): genau der nächste `take_single_use`-Aufruf für
    /// dieselbe Sitzung verbraucht den Eintrag.
    single_use: Vec<String>,
    /// Prozessweite Freigabe („volle Sandbox-Deaktivierung“, Nutzerwunsch
    /// 2026-09-21): gilt — anders als `approved_sessions` — für **jede**
    /// Session-ID desselben `harw`-Prozesses (Root-Session und jede
    /// Kind-Session), bis [`HostPermitSessionRegistry::revoke_global_approval`]
    /// sie entfernt oder ihr Fenster abläuft (B22).
    global_approval: Option<LeaseWindow>,
    /// Prozessweite Einmal-Freigabe: der nächste
    /// [`HostPermitSessionRegistry::take_single_use`]-Aufruf **jeder**
    /// Session-ID verbraucht sie, genau wie `single_use` es für eine
    /// einzelne Sitzung tut.
    global_single_use: bool,
}

impl RegistryState {
    // Gespeichertes (aktives oder abgelaufenes) Fenster der Sitzung.
    fn session_window(&self, session: &str) -> Option<LeaseWindow> {
        self.approved_sessions
            .iter()
            .find(|entry| entry.session == session)
            .map(|entry| entry.window)
    }

    // Alle zu `now` aktiven Fenster, die `session` abdecken (eigenes und
    // prozessweites).
    fn covering_windows(
        &self,
        session: &str,
        now: LeaseInstant,
    ) -> impl Iterator<Item = LeaseWindow> {
        self.session_window(session)
            .into_iter()
            .chain(self.global_approval)
            .filter(move |window| window.is_active_at(now))
    }

    // `true`, wenn zu `now` eine aktive Phase `session` abdeckt.
    fn is_covered_at(&self, session: &str, now: LeaseInstant) -> bool {
        self.covering_windows(session, now).next().is_some()
    }

    // Das zu `now` aktive Fenster mit der längsten Restlaufzeit.
    fn latest_covering(&self, session: &str, now: LeaseInstant) -> Option<LeaseWindow> {
        self.covering_windows(session, now)
            .max_by_key(|window| window.remaining_at(now))
    }
}

/// Verbindet eine einmalige lokale UI-Zustimmung mit den vielen einzelnen
/// [`ProcessPermitRequest`]s, die während einer Sitzung tatsächlich gestellt
/// werden.
///
/// # Description
/// Reiner In-Memory-Zustand für genau **einen** laufenden `harw`-Prozess
/// (Chat/TUI). Es gibt keine Persistenz, keine Datei und keinen IPC-Kanal zu
/// einem anderen Prozess — eine zweite `harw`-Instanz (z. B. ein separater
/// `harw sandbox`-Aufruf, siehe `harw-cli/src/sandbox_cmd.rs`) sieht diesen
/// Zustand nie, selbst wenn beide Prozesse dasselbe Projekt öffnen. Jede
/// Laufzeit-Montage (`harw_runtime::assembly::RuntimeAssembly::build`)
/// instanziiert genau eine Registry pro Lauf und reicht sie an jeden
/// `ShellToolProvider` mit `SandboxProfile::Host` weiter.
///
/// Jede Host-Arbeitsphase gilt höchstens [`Self::lease_ttl`] lang (Vorgabe
/// 8 h) und wird nie stillschweigend verlängert (siehe Moduldoku, Abschnitt
/// „Ablauf“).
///
/// # Concurrency
/// `Send + Sync`; ein einzelner [`Mutex`] schützt den gesamten Zustand
/// (`approved_sessions` und `remembered` liegen zusammen hinter derselben
/// Sperre, nie getrennt — eine Methode sieht damit nie einen Zwischenzustand
/// der anderen). Jede Methode hält die Sperre nur für die Dauer ihres eigenen
/// Aufrufs; es gibt keine Methode, die eine andere Methode dieses Typs
/// aufruft, während sie die Sperre bereits hält (kein Deadlock-Risiko
/// innerhalb des Typs). Ein vergifteter `Mutex` (ein anderer Aufrufer hat
/// unter Halten der Sperre paniert) lässt jede Methode fail-closed
/// zurückfallen (`false`/`None`/leerer `Vec`), statt selbst zu paniken.
#[derive(Debug)]
pub struct HostPermitSessionRegistry {
    state: Mutex<RegistryState>,
    /// Dauer jeder neuen Host-Arbeitsphase; nie null.
    lease_ttl: Duration,
}

impl Default for HostPermitSessionRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::new(RegistryState::default()),
            lease_ttl: DEFAULT_SESSION_LEASE_TTL,
        }
    }
}

impl HostPermitSessionRegistry {
    /// Vorgabe-Dauer einer Host-Arbeitsphase: 8 h (PL-100 Phase 4, B22).
    pub const DEFAULT_LEASE_TTL: Duration = DEFAULT_SESSION_LEASE_TTL;

    /// Erzeugt eine leere Registry, deren Host-Arbeitsphasen `lease_ttl`
    /// lang gelten.
    ///
    /// # Beschreibung
    /// Für die Montage, sobald eine Konfiguration die Dauer liefert, und für
    /// Tests. [`Self::default`] nutzt [`Self::DEFAULT_LEASE_TTL`].
    ///
    /// # Argumente
    /// - `lease_ttl` (`Duration`): Dauer jeder neuen Phase; muss größer als
    ///   null sein.
    ///
    /// # Errors
    /// [`ProcessPermitError::ZeroTtl`], wenn `lease_ttl` null ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitError};
    /// use std::time::Duration;
    ///
    /// let registry = HostPermitSessionRegistry::with_lease_ttl(Duration::from_secs(3600));
    /// assert!(registry.is_ok_and(|registry| registry.lease_ttl() == Duration::from_secs(3600)));
    /// assert!(matches!(
    ///     HostPermitSessionRegistry::with_lease_ttl(Duration::ZERO),
    ///     Err(ProcessPermitError::ZeroTtl)
    /// ));
    /// ```
    pub fn with_lease_ttl(lease_ttl: Duration) -> Result<Self, ProcessPermitError> {
        if lease_ttl.is_zero() {
            return Err(ProcessPermitError::ZeroTtl);
        }
        Ok(Self {
            state: Mutex::new(RegistryState::default()),
            lease_ttl,
        })
    }

    /// Dauer jeder neuen Host-Arbeitsphase dieser Registry.
    ///
    /// # Returns
    /// Den Wert aus [`Self::with_lease_ttl`] bzw. [`Self::DEFAULT_LEASE_TTL`].
    #[must_use]
    pub fn lease_ttl(&self) -> Duration {
        self.lease_ttl
    }

    // Neues Fenster ab `now`; protokolliert einen nicht darstellbaren
    // Endzeitpunkt (fail-closed: die Zustimmung wird dann nicht eingetragen).
    fn new_window(&self, now: LeaseInstant) -> Option<LeaseWindow> {
        let window = LeaseWindow::starting_at(now, self.lease_ttl);
        if window.is_none() {
            tracing::warn!(
                lease_ttl_secs = self.lease_ttl.as_secs(),
                "host lease end is not representable; approval not recorded"
            );
        }
        window
    }

    /// Merkt sich, dass die lokale UI Host-Ausführung für `session` erlaubt
    /// hat; „jetzt“ ist [`LeaseInstant::now`]. Siehe
    /// [`Self::mark_session_approved_at`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_session_approved("session-1");
    /// assert!(registry.is_session_approved("session-1"));
    /// ```
    pub fn mark_session_approved(&self, session: impl Into<String>) {
        self.mark_session_approved_at(session, LeaseInstant::now());
    }

    /// Merkt sich, dass die lokale UI Host-Ausführung für `session` erlaubt
    /// hat — für [`Self::lease_ttl`] ab `now`, oder bis der Nutzer die Phase
    /// vorher selbst beendet.
    ///
    /// # Beschreibung
    /// Wird genau einmal pro Sitzung aufgerufen, ausgelöst durch den
    /// Bestätigungsdialog. Danach darf der Aufrufer für jeden neuen,
    /// abweichenden Befehl innerhalb derselben Sitzung ohne erneutes Fragen
    /// einen eigenen Permit über [`crate::ProcessPermitLedger::issue_after_local_approval`]
    /// ausstellen lassen — die Zustimmung des Menschen bezog sich auf die
    /// Sitzung, nicht auf einen einzelnen Befehlstext.
    ///
    /// Ein erneuter Aufruf für dieselbe `session` legt keinen zweiten Eintrag
    /// an. Ist die Phase zu `now` noch aktiv, bleibt ihr Fenster unverändert
    /// (keine stillschweigende Verlängerung, B22). Ist sie abgelaufen, beginnt
    /// ein neues Fenster ab `now` (das ist die erneute Zustimmung nach dem
    /// Nachfragen). Die Zustimmung endet mit dem Ablauf, über
    /// [`Self::forget_session`] bzw. [`Self::revoke_session_approval`]
    /// (Strg+H bzw. `/sandbox-lease revoke`) oder mit dem Prozess.
    ///
    /// # Argumente
    /// - `session` (`impl Into<String>`): die Sitzungs-ID, die zugestimmt hat.
    /// - `now` (`LeaseInstant`): der Zeitpunkt der Zustimmung.
    ///
    /// # Panics
    /// Nie: ein vergifteter [`Mutex`] (ein Aufrufer hat unter Halten der Sperre
    /// paniert) wird über `if let Ok(..)` übersprungen, statt selbst zu
    /// paniken — die Zustimmung geht in diesem seltenen Fall verloren, aber
    /// der Aufrufer stürzt nicht ab. Ein nicht darstellbares Fensterende
    /// (`now + lease_ttl`) wird ebenso nicht eingetragen.
    ///
    /// # Concurrency
    /// `Send + Sync`; die Änderung läuft unter demselben [`Mutex`], der den
    /// gesamten Registrierungszustand schützt. Beliebig viele Threads dürfen
    /// gleichzeitig aufrufen; die Reihenfolge konkurrierender Aufrufe für
    /// dieselbe Sitzung ist die Sperrreihenfolge, nicht die Aufrufreihenfolge.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_session_approved("session-1");
    /// if let Some(window) = registry.session_lease("session-1") {
    ///     // Erneutes Markieren in der laufenden Phase verlängert nichts.
    ///     registry.mark_session_approved_at("session-1", window.granted_at());
    ///     assert_eq!(registry.session_lease("session-1"), Some(window));
    ///     // Nach dem Ablauf eröffnet die erneute Zustimmung ein neues Fenster.
    ///     registry.mark_session_approved_at("session-1", window.expires_at());
    ///     assert!(registry
    ///         .session_lease("session-1")
    ///         .is_some_and(|renewed| renewed.granted_at() == window.expires_at()));
    /// }
    /// ```
    pub fn mark_session_approved_at(&self, session: impl Into<String>, now: LeaseInstant) {
        let session = session.into();
        let Some(window) = self.new_window(now) else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        match state
            .approved_sessions
            .iter_mut()
            .find(|entry| entry.session == session)
        {
            // Aktive Phase: nie stillschweigend verlängern (B22).
            Some(entry) if entry.window.is_active_at(now) => {}
            // Abgelaufene Phase: die erneute Zustimmung eröffnet ein neues
            // Fenster.
            Some(entry) => entry.window = window,
            None => state
                .approved_sessions
                .push(ApprovedSession { session, window }),
        }
    }

    /// `true`, wenn `session` eine aktive lokale Host-Zustimmung hat; „jetzt“
    /// ist [`LeaseInstant::now`]. Siehe [`Self::is_session_approved_at`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// assert!(!registry.is_session_approved("unknown-session"));
    /// ```
    #[must_use]
    pub fn is_session_approved(&self, session: &str) -> bool {
        self.is_session_approved_at(session, LeaseInstant::now())
    }

    /// `true`, wenn `session` zu `now` eine aktive lokale Host-Zustimmung hat.
    ///
    /// # Beschreibung
    /// Rein lesend. Liefert zusätzlich `true`, wenn eine
    /// [`Self::mark_global_approval`]-Freigabe aktiv ist — diese gilt
    /// prozessweit, also auch für eine `session`, die nie selbst über
    /// [`Self::mark_session_approved`] zugestimmt hat (Root-Session und jede
    /// Kind-Session teilen sich dieselbe Registry-Instanz). Eine abgelaufene
    /// Phase (eine Uhr von `now` hat `expires_at` erreicht) zählt nicht (B22):
    /// der Aufrufer muss erneut fragen.
    ///
    /// # Argumente
    /// - `session` (`&str`): die zu prüfende Sitzungs-ID.
    /// - `now` (`LeaseInstant`): der Prüfzeitpunkt.
    ///
    /// # Returns
    /// `true`, wenn für `session` eine nicht widerrufene und nicht abgelaufene
    /// Zustimmung aus [`Self::mark_session_approved`] vorliegt, **oder** eine
    /// nicht widerrufene und nicht abgelaufene prozessweite Freigabe aus
    /// [`Self::mark_global_approval`] aktiv ist; sonst `false` — und ebenso
    /// `false`, wenn der interne [`Mutex`] vergiftet ist (fail-closed statt
    /// Panik).
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs. Sicher aus mehreren Threads gleichzeitig aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_session_approved("session-1");
    /// if let Some(window) = registry.session_lease("session-1") {
    ///     let t0 = window.granted_at();
    ///     assert!(registry.is_session_approved_at("session-1", t0));
    ///     assert!(!registry.is_session_approved_at("unknown-session", t0));
    ///     assert!(!registry.is_session_approved_at("session-1", window.expires_at()));
    /// }
    /// ```
    #[must_use]
    pub fn is_session_approved_at(&self, session: &str, now: LeaseInstant) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.is_covered_at(session, now))
    }

    /// Gespeichertes Fenster der sitzungseigenen Host-Arbeitsphase von
    /// `session`, aktiv oder abgelaufen.
    ///
    /// # Beschreibung
    /// Für Anzeigen („aktiv, endet in …“ bzw. „abgelaufen“). Die prozessweite
    /// Freigabe zählt hier nicht, siehe [`Self::global_lease`]. Für die
    /// Autorisierung gilt ausschließlich [`Self::is_session_approved_at`].
    ///
    /// # Returns
    /// `Some(window)` nach [`Self::mark_session_approved`] bis zum Widerruf;
    /// `None` für eine unbekannte oder widerrufene Sitzung und bei vergiftetem
    /// [`Mutex`].
    ///
    /// # Concurrency
    /// Sperrt kurz denselben [`Mutex`]; rein lesend.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// assert!(registry.session_lease("session-1").is_none());
    /// registry.mark_session_approved("session-1");
    /// assert!(registry
    ///     .session_lease("session-1")
    ///     .is_some_and(|w| w.ttl() == HostPermitSessionRegistry::DEFAULT_LEASE_TTL));
    /// ```
    #[must_use]
    pub fn session_lease(&self, session: &str) -> Option<LeaseWindow> {
        self.state.lock().ok()?.session_window(session)
    }

    /// Das aktive Fenster, das `session` abdeckt; „jetzt“ ist
    /// [`LeaseInstant::now`]. Siehe [`Self::covering_lease_at`].
    #[must_use]
    pub fn covering_lease(&self, session: &str) -> Option<LeaseWindow> {
        self.covering_lease_at(session, LeaseInstant::now())
    }

    /// Das Fenster der Host-Arbeitsphase, die `session` zu `now` abdeckt.
    ///
    /// # Beschreibung
    /// Berücksichtigt die sitzungseigene und die prozessweite Phase; decken
    /// beide ab, zählt die mit der längeren Restlaufzeit. Aufrufer, die unter
    /// einer laufenden Phase einen Permit ausstellen, reichen das Fenster an
    /// [`crate::ProcessPermitLedger::issue_for_lease_at`] weiter: der Permit
    /// endet dann mit der Phase, und ohne Fenster (`None`) wird nichts
    /// ausgestellt, statt still einen frischen 8-h-Permit zu erzeugen.
    ///
    /// # Argumente
    /// - `session` (`&str`): die Sitzungs-ID.
    /// - `now` (`LeaseInstant`): der Bezugszeitpunkt.
    ///
    /// # Returns
    /// `Some(window)` mit `window.is_active_at(now)`, wenn eine aktive Phase
    /// `session` abdeckt; sonst `None` — auch bei vergiftetem [`Mutex`]
    /// (fail-closed).
    ///
    /// # Concurrency
    /// Sperrt kurz denselben [`Mutex`]; rein lesend.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// assert!(registry.covering_lease("session-1").is_none());
    /// registry.mark_global_approval();
    /// let window = registry.covering_lease("session-1");
    /// assert!(window.is_some_and(|w| w.is_active_at(w.granted_at())));
    /// assert_eq!(window, registry.global_lease());
    /// ```
    #[must_use]
    pub fn covering_lease_at(&self, session: &str, now: LeaseInstant) -> Option<LeaseWindow> {
        self.state.lock().ok()?.latest_covering(session, now)
    }

    /// Restlaufzeit der Host-Arbeitsphase, die `session` abdeckt; „jetzt“ ist
    /// [`LeaseInstant::now`]. Siehe [`Self::lease_remaining_at`].
    #[must_use]
    pub fn lease_remaining(&self, session: &str) -> Option<Duration> {
        self.lease_remaining_at(session, LeaseInstant::now())
    }

    /// Restlaufzeit der Host-Arbeitsphase, die `session` zu `now` abdeckt.
    ///
    /// # Beschreibung
    /// Für Anzeigen („endet in …“). Zum Ausstellen eines Permits unter der
    /// Phase nicht als `ttl` verwenden (ein `None` würde dort einen vollen
    /// Permit ergeben), sondern [`Self::covering_lease_at`] mit
    /// [`crate::ProcessPermitLedger::issue_for_lease_at`].
    ///
    /// # Argumente
    /// - `session` (`&str`): die Sitzungs-ID.
    /// - `now` (`LeaseInstant`): der Bezugszeitpunkt.
    ///
    /// # Returns
    /// `Some(rest)` mit `rest > 0`, wenn eine aktive Phase `session` abdeckt
    /// (die längere Restlaufzeit, Minimum über beide Uhren); sonst `None` —
    /// auch bei vergiftetem [`Mutex`] (fail-closed).
    ///
    /// # Concurrency
    /// Sperrt kurz denselben [`Mutex`]; rein lesend.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// assert_eq!(registry.lease_remaining("session-1"), None);
    /// registry.mark_session_approved("session-1");
    /// if let Some(window) = registry.session_lease("session-1") {
    ///     assert_eq!(
    ///         registry.lease_remaining_at("session-1", window.granted_at()),
    ///         Some(HostPermitSessionRegistry::DEFAULT_LEASE_TTL)
    ///     );
    ///     assert_eq!(registry.lease_remaining_at("session-1", window.expires_at()), None);
    /// }
    /// ```
    #[must_use]
    pub fn lease_remaining_at(&self, session: &str, now: LeaseInstant) -> Option<Duration> {
        self.covering_lease_at(session, now)
            .map(|window| window.remaining_at(now))
    }

    /// Liefert die bereits gemerkte Permit-Kennung für einen exakt
    /// identischen Antrag, falls vorhanden; „jetzt“ ist [`LeaseInstant::now`].
    /// Siehe [`Self::lookup_permit_at`].
    ///
    /// # Beschreibung
    /// Der Vergleich läuft über [`PartialEq`] auf dem vollständigen
    /// [`ProcessPermitRequest`] (Sitzung, Worker-Definition, Befehlstext,
    /// Workspace, Umgebung) — ein auch nur im Befehlstext abweichender
    /// Antrag gilt als ein anderer Antrag und liefert `None`, selbst
    /// innerhalb derselben Sitzung.
    ///
    /// # Argumente
    /// - `request` (`&ProcessPermitRequest`): der exakt zu findende Antrag,
    ///   nur geliehen.
    ///
    /// # Returns
    /// `Some(id)`, wenn [`Self::remember_permit`] bereits für einen identischen
    /// `request` aufgerufen wurde und — falls der Permit unter einer
    /// Host-Arbeitsphase gemerkt wurde — weiterhin eine aktive Phase die
    /// Sitzung abdeckt; sonst `None` — auch bei vergiftetem [`Mutex`]
    /// (fail-closed).
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox ::{HostPermitSessionRegistry, ProcessEnvironment, request_for_workspace};
    /// use std::path::Path;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// let request = request_for_workspace(
    ///     "session-1",
    ///     "host-process-worker@1",
    ///     "echo hi",
    ///     Path::new("/workspace"),
    ///     ProcessEnvironment::LocalHost,
    /// );
    /// assert_eq!(registry.lookup_permit(&request), None);
    /// ```
    #[must_use]
    pub fn lookup_permit(&self, request: &ProcessPermitRequest) -> Option<ProcessPermitId> {
        self.lookup_permit_at(request, LeaseInstant::now())
    }

    /// Liefert die gemerkte Permit-Kennung für einen exakt identischen Antrag
    /// zu `now`.
    ///
    /// # Beschreibung
    /// Wie [`Self::lookup_permit`]. Ein Eintrag, der unter einer
    /// Host-Arbeitsphase gemerkt wurde, wird nur geliefert, solange zu `now`
    /// eine aktive Phase (sitzungseigen oder prozessweit) seine Sitzung
    /// abdeckt. Nach dem Ablauf oder Widerruf der Phase liefert diese Methode
    /// `None`; der Aufrufer prüft dann die Phase und fragt erneut (B22). Ein
    /// ohne Phase gemerkter Eintrag (Einmalfreigabe) bleibt unberührt; seine
    /// Gültigkeit prüft allein der [`crate::ProcessPermitLedger`].
    ///
    /// # Argumente
    /// - `request` (`&ProcessPermitRequest`): der exakt zu findende Antrag.
    /// - `now` (`LeaseInstant`): der Prüfzeitpunkt.
    ///
    /// # Returns
    /// `Some(id)` oder `None`, siehe oben; `None` auch bei vergiftetem
    /// [`Mutex`].
    ///
    /// # Concurrency
    /// Sperrt kurz denselben [`Mutex`]; rein lesend.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::{HostPermitSessionRegistry, ProcessEnvironment, request_for_workspace};
    /// use std::path::Path;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// let request = request_for_workspace(
    ///     "session-1",
    ///     "host-process-worker@1",
    ///     "echo hi",
    ///     Path::new("/workspace"),
    ///     ProcessEnvironment::LocalHost,
    /// );
    /// registry.mark_session_approved("session-1");
    /// if let Some(window) = registry.session_lease("session-1") {
    ///     assert_eq!(registry.lookup_permit_at(&request, window.granted_at()), None);
    /// }
    /// ```
    #[must_use]
    pub fn lookup_permit_at(
        &self,
        request: &ProcessPermitRequest,
        now: LeaseInstant,
    ) -> Option<ProcessPermitId> {
        let state = self.state.lock().ok()?;
        let entry = state
            .remembered
            .iter()
            .find(|entry| &entry.request == request)?;
        if entry.lease_bound && !state.is_covered_at(&entry.request.session, now) {
            return None;
        }
        Some(entry.id)
    }

    /// Merkt sich eine frisch ausgestellte Permit-Kennung für ihren exakten
    /// Antrag, damit derselbe Antrag später ohne erneute Ausstellung
    /// autorisiert werden kann; „jetzt“ ist [`LeaseInstant::now`]. Siehe
    /// [`Self::remember_permit_at`].
    ///
    /// # Beschreibung
    /// Ein bereits gemerkter Eintrag für einen identischen `request` wird
    /// vollständig durch den neuen ersetzt (kein Anhäufen mehrerer Kennungen
    /// für denselben Antrag). Der Aufrufer ist dafür verantwortlich, dass
    /// `id` tatsächlich zu `request` passt — diese Methode prüft das nicht,
    /// das übernimmt [`crate::ProcessPermitLedger::authorize`] beim
    /// nächsten [`Self::lookup_permit`]-gestützten Aufruf.
    ///
    /// # Argumente
    /// - `request` (`ProcessPermitRequest`): der Antrag, dessen Kennung
    ///   gemerkt wird; Eigentum geht über.
    /// - `id` (`ProcessPermitId`): die bereits beim Ledger ausgestellte
    ///   Kennung für genau diesen Antrag.
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs. Bei vergiftetem `Mutex` verwirft dieser Aufruf sich
    /// selbst, statt zu paniken — die Kennung geht dann verloren und ein
    /// späterer [`Self::lookup_permit`] liefert `None`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::{
    ///     HostApprovalScope, HostPermitSessionRegistry, ProcessEnvironment, ProcessPermitError,
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
    /// let id = ledger.issue_after_local_approval(
    ///     request.clone(),
    ///     HostApprovalScope::SessionLease,
    ///     None,
    /// )?;
    /// registry.remember_permit(request.clone(), id);
    /// assert_eq!(registry.lookup_permit(&request), Some(id));
    /// # Ok(())
    /// # }
    /// ```
    pub fn remember_permit(&self, request: ProcessPermitRequest, id: ProcessPermitId) {
        self.remember_permit_at(request, id, LeaseInstant::now());
    }

    /// Merkt sich eine Permit-Kennung für ihren exakten Antrag zu `now`.
    ///
    /// # Beschreibung
    /// Wie [`Self::remember_permit`]. Deckt zu `now` eine aktive
    /// Host-Arbeitsphase die Sitzung von `request` ab, wird der Eintrag an
    /// Phasen gebunden: [`Self::lookup_permit_at`] liefert ihn nur, solange
    /// eine Phase die Sitzung abdeckt (B22).
    ///
    /// # Argumente
    /// - `request` (`ProcessPermitRequest`): der Antrag; Eigentum geht über.
    /// - `id` (`ProcessPermitId`): die beim Ledger ausgestellte Kennung.
    /// - `now` (`LeaseInstant`): der Zeitpunkt des Merkens.
    ///
    /// # Concurrency
    /// Sperrt kurz denselben [`Mutex`]. Bei vergiftetem `Mutex` geht die
    /// Kennung verloren (keine Panik).
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::{
    ///     HostApprovalScope, HostPermitSessionRegistry, ProcessEnvironment, ProcessPermitError,
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
    /// // Ohne aktive Phase gibt es kein Fenster und damit keinen Permit.
    /// let window = registry
    ///     .covering_lease("session-1")
    ///     .ok_or(ProcessPermitError::Expired)?;
    /// let t0 = window.granted_at();
    /// let id = ledger.issue_for_lease_at(request.clone(), window, t0)?;
    /// registry.remember_permit_at(request.clone(), id, t0);
    /// assert_eq!(registry.lookup_permit_at(&request, t0), Some(id));
    /// // Nach dem Ende der Phase liefert die Registry den Permit nicht mehr.
    /// assert_eq!(registry.lookup_permit_at(&request, window.expires_at()), None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn remember_permit_at(
        &self,
        request: ProcessPermitRequest,
        id: ProcessPermitId,
        now: LeaseInstant,
    ) {
        if let Ok(mut state) = self.state.lock() {
            let lease_bound = state.is_covered_at(&request.session, now);
            state.remembered.retain(|entry| entry.request != request);
            state.remembered.push(RememberedPermit {
                request,
                id,
                lease_bound,
            });
        }
    }

    /// Entfernt jede Zustimmung und jeden gemerkten Permit dieser Sitzung,
    /// etwa bei Sitzungsende oder explizitem Widerruf über `/sandbox`.
    ///
    /// # Beschreibung
    /// Entfernt sowohl den Eintrag aus [`Self::mark_session_approved`] (die
    /// Sitzung muss danach erneut zustimmen) als auch jeden über
    /// [`Self::remember_permit`] gemerkten Antrag dieser Sitzung. Diese
    /// Registry widerruft dabei **nicht** selbst die zugehörigen Permits im
    /// [`crate::ProcessPermitLedger`] — sie kennt den Ledger nicht (siehe
    /// Moduldoku, Abschnitt „Verantwortungsgrenze“). Der Aufrufer muss die
    /// zurückgegebenen Kennungen selbst an [`crate::ProcessPermitLedger::revoke`]
    /// übergeben.
    ///
    /// # Argumente
    /// - `session` (`&str`): die zu vergessende Sitzungs-ID.
    ///
    /// # Returns
    /// Die Kennungen (`Vec<`[`ProcessPermitId`]`>`) aller entfernten gemerkten
    /// Permit-Einträge dieser Sitzung — leer, wenn die Sitzung unbekannt war,
    /// nichts gemerkt hatte, oder der interne [`Mutex`] vergiftet ist
    /// (fail-closed: kein Fehler, aber auch keine Kennung zum Widerrufen).
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_session_approved("session-1");
    /// let removed = registry.forget_session("session-1");
    /// assert!(removed.is_empty(), "keine Permits waren gemerkt");
    /// assert!(!registry.is_session_approved("session-1"));
    /// ```
    pub fn forget_session(&self, session: &str) -> Vec<ProcessPermitId> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        state
            .approved_sessions
            .retain(|entry| entry.session != session);
        let mut removed_ids = Vec::new();
        state.remembered.retain(|entry| {
            if entry.request.session == session {
                removed_ids.push(entry.id);
                false
            } else {
                true
            }
        });
        removed_ids
    }

    /// Merkt sich, dass die lokale UI Host-Ausführung für `session` genau
    /// einmal für den nächsten Befehl erlaubt hat
    /// (`HostApprovalScope::SingleExecution`-Zustimmung im
    /// Bestätigungsdialog).
    ///
    /// # Beschreibung
    /// Im Unterschied zu [`Self::mark_session_approved`] gilt diese
    /// Zustimmung nicht für die restliche Sitzung, sondern für genau den
    /// nächsten Befehl: [`Self::take_single_use`] verbraucht sie beim ersten
    /// Aufruf und liefert danach `false`, bis erneut `mark_single_use`
    /// aufgerufen wird. Ein wiederholter Aufruf für dieselbe `session`, bevor
    /// sie verbraucht wurde, häuft keinen zweiten Eintrag an (idempotent).
    ///
    /// # Argumente
    /// - `session` (`String`): die Sitzungs-ID, die einmalig zugestimmt hat.
    ///
    /// # Panics
    /// Nie: ein vergifteter [`Mutex`] wird über `if let Ok(..)` übersprungen,
    /// statt selbst zu paniken.
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_single_use("session-1".to_owned());
    /// assert!(registry.has_single_use("session-1"));
    /// ```
    pub fn mark_single_use(&self, session: String) {
        if let Ok(mut state) = self.state.lock()
            && !state.single_use.iter().any(|existing| existing == &session)
        {
            state.single_use.push(session);
        }
    }

    /// Verbraucht die Einmal-Freigabe von `session` atomar: liefert `true`
    /// genau beim ersten Aufruf nach [`Self::mark_single_use`] **oder**
    /// [`Self::mark_global_single_use`], danach `false`.
    ///
    /// # Beschreibung
    /// Prüfung und Entfernen laufen unter derselben Sperre als ein einziger
    /// Schritt — zwei nebenläufige Aufrufe für dieselbe `session` (oder für
    /// verschiedene Sitzungen, wenn nur eine globale Einmal-Freigabe aktiv
    /// ist) können daher nie beide `true` liefern, selbst wenn sie exakt
    /// gleichzeitig eintreffen. Die sitzungseigene Freigabe hat Vorrang: ist
    /// sowohl eine sitzungseigene als auch eine globale Einmal-Freigabe
    /// aktiv, verbraucht dieser Aufruf zuerst die sitzungseigene und lässt
    /// die globale für die nächste beliebige Sitzung übrig.
    ///
    /// # Argumente
    /// - `session` (`&str`): die zu verbrauchende Sitzungs-ID.
    ///
    /// # Returns
    /// `true`, wenn `session` eine noch nicht verbrauchte sitzungseigene
    /// Einmal-Freigabe hatte, **oder** — falls nicht — eine noch nicht
    /// verbrauchte prozessweite Einmal-Freigabe aktiv war (die damit jetzt
    /// verbraucht ist); `false`, wenn weder eine sitzungseigene noch eine
    /// globale Einmal-Freigabe vorlag — und ebenso `false` bei vergiftetem
    /// [`Mutex`] (fail-closed).
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs. Sicher aus mehreren Threads gleichzeitig aufrufbar —
    /// genau ein gleichzeitiger Aufrufer erhält `true` für dieselbe
    /// sitzungseigene bzw. dieselbe globale Freigabe.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_single_use("session-1".to_owned());
    /// assert!(registry.take_single_use("session-1"));
    /// assert!(!registry.take_single_use("session-1"));
    /// ```
    #[must_use]
    pub fn take_single_use(&self, session: &str) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if let Some(position) = state
            .single_use
            .iter()
            .position(|existing| existing == session)
        {
            state.single_use.remove(position);
            return true;
        }
        if state.global_single_use {
            state.global_single_use = false;
            return true;
        }
        false
    }

    /// `true`, wenn `session` noch eine unverbrauchte Einmal-Freigabe hat,
    /// ohne sie zu verbrauchen.
    ///
    /// # Beschreibung
    /// Berücksichtigt neben der sitzungseigenen auch eine aktive
    /// prozessweite Einmal-Freigabe aus [`Self::mark_global_single_use`] —
    /// diese gilt für jede Session-ID, nicht nur für die, die sie ausgelöst
    /// hat.
    ///
    /// # Argumente
    /// - `session` (`&str`): die zu prüfende Sitzungs-ID.
    ///
    /// # Returns
    /// `true`, wenn [`Self::mark_single_use`] für `session` aufgerufen wurde
    /// und [`Self::take_single_use`] den Eintrag noch nicht verbraucht hat,
    /// **oder** eine noch nicht verbrauchte globale Einmal-Freigabe aktiv
    /// ist; sonst `false` — auch bei vergiftetem [`Mutex`] (fail-closed).
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs. Rein lesend: verändert den Zustand nicht.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// assert!(!registry.has_single_use("session-1"));
    /// registry.mark_single_use("session-1".to_owned());
    /// assert!(registry.has_single_use("session-1"));
    /// ```
    #[must_use]
    pub fn has_single_use(&self, session: &str) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        state.global_single_use || state.single_use.iter().any(|existing| existing == session)
    }

    /// Entfernt sowohl die Sitzungsfreigabe (siehe
    /// [`Self::mark_session_approved`]) als auch eine gemerkte
    /// Einmal-Freigabe (siehe [`Self::mark_single_use`]) von `session`.
    ///
    /// # Beschreibung
    /// Idempotent: ein zweiter Aufruf für dieselbe oder eine unbekannte
    /// `session` ändert nichts und liefert keinen Fehler. Im Unterschied zu
    /// [`Self::forget_session`] rührt diese Methode keine gemerkten
    /// [`ProcessPermitId`]s an — ausschließlich zum expliziten Widerruf einer
    /// Host-Freigabe über `/sandbox-lease revoke`, nicht zum Aufräumen bei
    /// Sitzungsende.
    ///
    /// # Argumente
    /// - `session` (`&str`): die Sitzungs-ID, deren Freigaben entfernt werden.
    ///
    /// # Panics
    /// Nie: ein vergifteter [`Mutex`] wird über `if let Ok(..)` übersprungen,
    /// statt selbst zu paniken.
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_session_approved("session-1");
    /// registry.mark_single_use("session-1".to_owned());
    /// registry.revoke_session_approval("session-1");
    /// assert!(!registry.is_session_approved("session-1"));
    /// assert!(!registry.has_single_use("session-1"));
    /// // Zweiter Aufruf ist ein No-Op statt eines Fehlers.
    /// registry.revoke_session_approval("session-1");
    /// ```
    pub fn revoke_session_approval(&self, session: &str) {
        if let Ok(mut state) = self.state.lock() {
            state
                .approved_sessions
                .retain(|entry| entry.session != session);
            state.single_use.retain(|existing| existing != session);
        }
    }

    /// Merkt sich eine prozessweite Host-Ausführungsfreigabe; „jetzt“ ist
    /// [`LeaseInstant::now`]. Siehe [`Self::mark_global_approval_at`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_global_approval();
    /// assert!(registry.is_session_approved("any-session-id"));
    /// ```
    pub fn mark_global_approval(&self) {
        self.mark_global_approval_at(LeaseInstant::now());
    }

    /// Merkt sich eine prozessweite Host-Ausführungsfreigabe („volle
    /// Sandbox-Deaktivierung“, Nutzerwunsch 2026-09-21) ab `now`: gilt
    /// [`Self::lease_ttl`] lang bzw. bis zum Widerruf durch den Nutzer
    /// ([`Self::revoke_global_approval`]) für **jede** Session-ID desselben
    /// `harw`-Prozesses — die Root-Session und jede Kind-Session, die dieselbe
    /// Registry-Instanz teilt (siehe
    /// `harw_registry_defaults::profile::build_shell_provider`, das
    /// Ledger/Registry/Fragekanal jetzt an jeden `ShellToolProvider` hängt,
    /// nicht nur an einen mit `SandboxProfile::Host`).
    ///
    /// # Beschreibung
    /// Ist die prozessweite Freigabe zu `now` noch aktiv, ändert ein erneuter
    /// Aufruf nichts (keine stillschweigende Verlängerung, B22). Ist sie
    /// abgelaufen oder widerrufen, beginnt ein neues Fenster ab `now`. Die
    /// Freigabe endet mit dem Ablauf, über [`Self::revoke_global_approval`]
    /// (Strg+H bzw. `/sandbox-lease revoke`) oder mit dem Prozess.
    ///
    /// # Argumente
    /// - `now` (`LeaseInstant`): der Zeitpunkt der Zustimmung.
    ///
    /// # Panics
    /// Nie: ein vergifteter [`Mutex`] wird übersprungen, statt selbst zu
    /// paniken — die Freigabe geht in diesem seltenen Fall verloren, aber der
    /// Aufrufer stürzt nicht ab. Ein nicht darstellbares Fensterende wird
    /// ebenso nicht eingetragen.
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_global_approval();
    /// if let Some(window) = registry.global_lease() {
    ///     // In der laufenden Phase: keine Verlängerung.
    ///     registry.mark_global_approval_at(window.granted_at());
    ///     assert_eq!(registry.global_lease(), Some(window));
    ///     assert!(registry.has_global_approval_at(window.granted_at()));
    /// }
    /// ```
    pub fn mark_global_approval_at(&self, now: LeaseInstant) {
        let Some(window) = self.new_window(now) else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        // Aktive Phase: nie stillschweigend verlängern (B22).
        if state
            .global_approval
            .is_some_and(|existing| existing.is_active_at(now))
        {
            return;
        }
        state.global_approval = Some(window);
    }

    /// `true`, solange die prozessweite Freigabe aktiv ist; „jetzt“ ist
    /// [`LeaseInstant::now`]. Siehe [`Self::has_global_approval_at`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// assert!(!registry.has_global_approval());
    /// registry.mark_global_approval();
    /// assert!(registry.has_global_approval());
    /// ```
    #[must_use]
    pub fn has_global_approval(&self) -> bool {
        self.has_global_approval_at(LeaseInstant::now())
    }

    /// `true`, wenn die prozessweite Freigabe aus
    /// [`Self::mark_global_approval`] zu `now` aktiv ist (weder widerrufen
    /// noch abgelaufen).
    ///
    /// # Argumente
    /// - `now` (`LeaseInstant`): der Prüfzeitpunkt.
    ///
    /// # Returns
    /// `true` nach [`Self::mark_global_approval`], vor
    /// [`Self::revoke_global_approval`] und vor dem Fensterende; sonst
    /// `false` — auch bei vergiftetem [`Mutex`] (fail-closed).
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs. Rein lesend.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_global_approval();
    /// if let Some(window) = registry.global_lease() {
    ///     assert!(registry.has_global_approval_at(window.granted_at()));
    ///     assert!(!registry.has_global_approval_at(window.expires_at()));
    /// }
    /// ```
    #[must_use]
    pub fn has_global_approval_at(&self, now: LeaseInstant) -> bool {
        self.state.lock().is_ok_and(|state| {
            state
                .global_approval
                .is_some_and(|window| window.is_active_at(now))
        })
    }

    /// Gespeichertes Fenster der prozessweiten Freigabe, aktiv oder
    /// abgelaufen.
    ///
    /// # Beschreibung
    /// Für Anzeigen („aktiv, endet in …“ bzw. „abgelaufen“). Für die
    /// Autorisierung gilt ausschließlich [`Self::has_global_approval_at`].
    ///
    /// # Returns
    /// `Some(window)` nach [`Self::mark_global_approval`] bis
    /// [`Self::revoke_global_approval`]; sonst `None` — auch bei vergiftetem
    /// [`Mutex`].
    ///
    /// # Concurrency
    /// Sperrt kurz denselben [`Mutex`]; rein lesend.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// assert!(registry.global_lease().is_none());
    /// registry.mark_global_approval();
    /// assert!(registry.global_lease().is_some());
    /// ```
    #[must_use]
    pub fn global_lease(&self) -> Option<LeaseWindow> {
        self.state.lock().ok()?.global_approval
    }

    /// Merkt sich eine prozessweite Einmal-Freigabe: der nächste
    /// [`Self::take_single_use`]-Aufruf einer **beliebigen** Session-ID
    /// verbraucht sie.
    ///
    /// # Beschreibung
    /// Idempotent wie [`Self::mark_single_use`]: ein wiederholter Aufruf,
    /// bevor die Freigabe verbraucht wurde, häuft keinen zweiten Zustand an
    /// (es gibt ohnehin nur ein einziges `bool`-Flag).
    ///
    /// # Panics
    /// Nie: ein vergifteter [`Mutex`] wird über `if let Ok(..)` übersprungen,
    /// statt selbst zu paniken.
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_global_single_use();
    /// assert!(registry.has_global_single_use());
    /// ```
    pub fn mark_global_single_use(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.global_single_use = true;
        }
    }

    /// `true`, wenn eine prozessweite Einmal-Freigabe aus
    /// [`Self::mark_global_single_use`] noch nicht verbraucht wurde, ohne sie
    /// zu verbrauchen.
    ///
    /// # Returns
    /// `true`, wenn [`Self::mark_global_single_use`] aufgerufen wurde und
    /// [`Self::take_single_use`] (für keine Session-ID) die globale Freigabe
    /// noch nicht verbraucht hat; sonst `false` — auch bei vergiftetem
    /// [`Mutex`] (fail-closed).
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs. Rein lesend: verändert den Zustand nicht.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// assert!(!registry.has_global_single_use());
    /// registry.mark_global_single_use();
    /// assert!(registry.has_global_single_use());
    /// ```
    #[must_use]
    pub fn has_global_single_use(&self) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        state.global_single_use
    }

    /// Entfernt sowohl die prozessweite Sitzungsfreigabe (siehe
    /// [`Self::mark_global_approval`]) als auch eine gemerkte prozessweite
    /// Einmal-Freigabe (siehe [`Self::mark_global_single_use`]).
    ///
    /// # Beschreibung
    /// Idempotent: ein zweiter Aufruf ändert nichts und liefert keinen
    /// Fehler — dasselbe Verhalten wie [`Self::revoke_session_approval`], nur
    /// für den globalen statt den sitzungseigenen Zustand. Rührt weder
    /// `approved_sessions` noch `single_use` noch `remembered` an: eine
    /// zusätzlich aktive sitzungseigene Freigabe bleibt nach diesem Aufruf
    /// bestehen, nur die prozessweite Wirkung endet.
    ///
    /// # Panics
    /// Nie: ein vergifteter [`Mutex`] wird über `if let Ok(..)` übersprungen,
    /// statt selbst zu paniken.
    ///
    /// # Concurrency
    /// `Send + Sync`; sperrt kurz denselben [`Mutex`] wie jede andere Methode
    /// dieses Typs.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_global_approval();
    /// registry.mark_global_single_use();
    /// registry.revoke_global_approval();
    /// assert!(!registry.is_session_approved("any-session-id"));
    /// assert!(!registry.has_global_single_use());
    /// ```
    pub fn revoke_global_approval(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.global_approval = None;
            state.global_single_use = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_permit::{
        HostApprovalScope, ProcessEnvironment, ProcessPermitLedger, request_for_workspace,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use std::path::Path;

    // Dauer in Stunden, gesättigt statt überlaufend.
    fn hours(count: u64) -> Duration {
        Duration::from_secs(count.saturating_mul(60 * 60))
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

    // Registry mit kurzer, eigener Phasendauer.
    fn registry_with_ttl(ttl: Duration) -> TestResult<HostPermitSessionRegistry> {
        HostPermitSessionRegistry::with_lease_ttl(ttl).map_err(ctx("with_lease_ttl failed"))
    }

    fn request(session: &str, command: &str) -> ProcessPermitRequest {
        request_for_workspace(
            session,
            "host-process-worker@1",
            command,
            Path::new("/workspace"),
            ProcessEnvironment::LocalHost,
        )
    }

    // Kein Feld von `ProcessPermitId` ist außerhalb von `process_permit.rs`
    // sichtbar; Tests holen sich echte Kennungen deshalb über den echten
    // Ledger statt sie zu konstruieren.
    fn issue(
        ledger: &ProcessPermitLedger,
        request: &ProcessPermitRequest,
    ) -> TestResult<ProcessPermitId> {
        ledger
            .issue_after_local_approval(request.clone(), HostApprovalScope::SessionLease, None)
            .map_err(|e| TestError::Context {
                context: "issuing a valid host request must succeed",
                source: e.to_string(),
            })
    }

    #[test]
    fn test_is_session_approved_before_and_after_mark_session_approved() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.is_session_approved("s1"));
        registry.mark_session_approved("s1");
        assert!(registry.is_session_approved("s1"));
        assert!(!registry.is_session_approved("s2"));
    }

    #[test]
    fn test_default_lease_ttl_is_eight_hours_and_zero_is_rejected() {
        assert_eq!(HostPermitSessionRegistry::DEFAULT_LEASE_TTL, hours(8));
        assert_eq!(
            HostPermitSessionRegistry::default().lease_ttl(),
            HostPermitSessionRegistry::DEFAULT_LEASE_TTL
        );
        assert!(matches!(
            HostPermitSessionRegistry::with_lease_ttl(Duration::ZERO),
            Err(ProcessPermitError::ZeroTtl)
        ));
    }

    #[test]
    fn test_session_lease_expires_at_boundary() -> TestResult {
        // PL-100 Phase 4 (B22): ersetzt `test_session_approval_does_not_expire_over_time`
        // (Nutzerentscheidung 2026-09-24, kein Zeitablauf).
        let registry = HostPermitSessionRegistry::default();
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        let boundary = later(t0, HostPermitSessionRegistry::DEFAULT_LEASE_TTL)?;

        let window = registry
            .session_lease("s1")
            .ok_or(TestError::Missing("session lease window"))?;
        assert_eq!(window.granted_at(), t0);
        assert_eq!(window.expires_at(), boundary);

        assert!(registry.is_session_approved_at("s1", t0));
        assert!(registry.is_session_approved_at("s1", later(t0, hours(7))?));
        assert!(registry.is_session_approved_at("s1", just_before(boundary)?));
        assert!(
            !registry.is_session_approved_at("s1", boundary),
            "now == expires_at must count as expired"
        );
        Ok(())
    }

    #[test]
    fn test_expired_session_lease_is_absent_and_forces_reapproval() -> TestResult {
        let registry = registry_with_ttl(hours(1))?;
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        let t2 = later(t0, hours(2))?;

        assert!(
            !registry.is_session_approved_at("s1", t2),
            "an expired lease must be treated as absent"
        );
        assert_eq!(registry.lease_remaining_at("s1", t2), None);
        // Das abgelaufene Fenster bleibt für Anzeigen sichtbar, wirkt aber nicht.
        let expired = registry
            .session_lease("s1")
            .ok_or(TestError::Missing("expired window kept for display"))?;
        assert!(!expired.is_active_at(t2));
        assert_eq!(expired.remaining_at(t2), Duration::ZERO);

        // Erneute Zustimmung nach dem Nachfragen: neues Fenster ab `t2`.
        registry.mark_session_approved_at("s1", t2);
        assert!(registry.is_session_approved_at("s1", t2));
        let renewed = registry
            .session_lease("s1")
            .ok_or(TestError::Missing("renewed window"))?;
        assert_eq!(renewed.granted_at(), t2);
        assert_eq!(renewed.expires_at(), later(t2, hours(1))?);
        Ok(())
    }

    #[test]
    fn test_mark_session_approved_does_not_extend_active_lease() -> TestResult {
        let registry = HostPermitSessionRegistry::default();
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        // Ein erneutes Markieren kurz vor dem Ende verlängert nichts.
        registry.mark_session_approved_at("s1", later(t0, hours(7))?);
        let window = registry
            .session_lease("s1")
            .ok_or(TestError::Missing("session lease window"))?;
        assert_eq!(window.granted_at(), t0);
        assert_eq!(window.expires_at(), later(t0, hours(8))?);
        assert!(!registry.is_session_approved_at("s1", later(t0, hours(8))?));
        Ok(())
    }

    #[test]
    fn test_revoke_removes_expired_session_lease_record() -> TestResult {
        let registry = registry_with_ttl(hours(1))?;
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        registry.mark_session_approved_at("s2", t0);
        assert!(!registry.is_session_approved_at("s1", later(t0, hours(2))?));

        registry.revoke_session_approval("s1");
        assert!(registry.session_lease("s1").is_none());
        let removed = registry.forget_session("s2");
        assert!(removed.is_empty());
        assert!(registry.session_lease("s2").is_none());
        Ok(())
    }

    #[test]
    fn test_unrepresentable_lease_end_fails_closed() -> TestResult {
        // `now + lease_ttl` ist nicht darstellbar: die Zustimmung wird nicht
        // eingetragen, statt eine Phase ohne Ende anzulegen.
        let registry = registry_with_ttl(Duration::MAX)?;
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        registry.mark_global_approval_at(t0);
        assert!(!registry.is_session_approved_at("s1", t0));
        assert!(!registry.has_global_approval_at(t0));
        assert!(registry.session_lease("s1").is_none());
        assert!(registry.global_lease().is_none());
        Ok(())
    }

    #[test]
    fn test_mark_session_approved_is_idempotent_and_one_revoke_ends_it() {
        let registry = HostPermitSessionRegistry::default();
        registry.mark_session_approved("s1");
        registry.mark_session_approved("s1");
        registry.revoke_session_approval("s1");
        assert!(
            !registry.is_session_approved("s1"),
            "a repeated mark must not leave a second entry behind a single revoke"
        );
    }

    #[test]
    fn test_remembered_permit_is_found_only_for_identical_request() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let registry = HostPermitSessionRegistry::default();
        let req = request("s1", "echo hi");
        assert_eq!(registry.lookup_permit(&req), None);
        let id = issue(&ledger, &req)?;
        registry.remember_permit(req.clone(), id);
        assert_eq!(registry.lookup_permit(&req), Some(id));

        let other = request("s1", "echo bye");
        assert_eq!(registry.lookup_permit(&other), None);
        Ok(())
    }

    #[test]
    fn test_forget_session_clears_approval_and_returns_removed_ids() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let registry = HostPermitSessionRegistry::default();
        registry.mark_session_approved("s1");
        let req = request("s1", "echo hi");
        let id = issue(&ledger, &req)?;
        registry.remember_permit(req.clone(), id);

        let removed = registry.forget_session("s1");
        assert_eq!(removed, vec![id]);
        assert!(!registry.is_session_approved("s1"));
        assert_eq!(registry.lookup_permit(&req), None);
        Ok(())
    }

    #[test]
    fn test_take_single_use_returns_true_exactly_once() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.take_single_use("s1"));
        registry.mark_single_use("s1".to_owned());
        assert!(registry.take_single_use("s1"));
        assert!(
            !registry.take_single_use("s1"),
            "a second take for the same session must not succeed"
        );
    }

    #[test]
    fn test_has_single_use_reflects_state_without_consuming() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.has_single_use("s1"));
        registry.mark_single_use("s1".to_owned());
        assert!(registry.has_single_use("s1"));
        assert!(
            registry.has_single_use("s1"),
            "has_single_use must not consume the entry"
        );
        assert!(registry.take_single_use("s1"));
        assert!(!registry.has_single_use("s1"));
    }

    #[test]
    fn test_mark_single_use_is_idempotent_before_consumption() {
        let registry = HostPermitSessionRegistry::default();
        registry.mark_single_use("s1".to_owned());
        registry.mark_single_use("s1".to_owned());
        assert!(registry.take_single_use("s1"));
        assert!(
            !registry.take_single_use("s1"),
            "a repeated mark before consumption must not grant a second use"
        );
    }

    #[test]
    fn test_revoke_session_approval_removes_session_and_single_use_and_is_idempotent() {
        let registry = HostPermitSessionRegistry::default();
        registry.mark_session_approved("s1");
        registry.mark_single_use("s1".to_owned());

        registry.revoke_session_approval("s1");
        assert!(!registry.is_session_approved("s1"));
        assert!(!registry.has_single_use("s1"));

        // Zweiter Aufruf ist ein No-Op statt eines Fehlers oder einer Panik.
        registry.revoke_session_approval("s1");
        assert!(!registry.is_session_approved("s1"));
    }

    #[test]
    fn test_revoke_session_approval_on_unknown_session_is_a_no_op() {
        let registry = HostPermitSessionRegistry::default();
        registry.revoke_session_approval("unknown");
        assert!(!registry.is_session_approved("unknown"));
    }

    // ── Prozessweite Freigabe (Nutzerwunsch „volle Sandbox-Deaktivierung“,
    // 2026-09-21) ────────────────────────────────────────────────────────────

    #[test]
    fn test_mark_global_approval_is_seen_by_a_session_that_never_approved_itself() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.is_session_approved("foreign-session"));
        registry.mark_global_approval();
        assert!(
            registry.is_session_approved("foreign-session"),
            "a global approval must cover every session id, not just the one that requested it"
        );
        assert!(registry.is_session_approved("another-foreign-session"));
    }

    #[test]
    fn test_has_global_approval_before_and_after_mark() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.has_global_approval());
        registry.mark_global_approval();
        assert!(registry.has_global_approval());
    }

    #[test]
    fn test_expired_global_lease_is_absent() -> TestResult {
        // PL-100 Phase 4 (B22): ersetzt `test_global_approval_does_not_expire_over_time`.
        let registry = HostPermitSessionRegistry::default();
        let t0 = LeaseInstant::now();
        registry.mark_global_approval_at(t0);
        let boundary = later(t0, HostPermitSessionRegistry::DEFAULT_LEASE_TTL)?;

        assert!(registry.has_global_approval_at(just_before(boundary)?));
        assert!(registry.is_session_approved_at("foreign-session", just_before(boundary)?));
        assert!(!registry.has_global_approval_at(boundary));
        assert!(
            !registry.is_session_approved_at("foreign-session", boundary),
            "an expired global lease must not cover any session"
        );
        let window = registry
            .global_lease()
            .ok_or(TestError::Missing("expired global window kept for display"))?;
        assert!(!window.is_active_at(boundary));

        registry.revoke_global_approval();
        assert!(registry.global_lease().is_none());
        Ok(())
    }

    #[test]
    fn test_mark_global_approval_does_not_extend_active_lease_but_renews_expired() -> TestResult {
        let registry = registry_with_ttl(hours(1))?;
        let t0 = LeaseInstant::now();
        registry.mark_global_approval_at(t0);
        registry.mark_global_approval_at(later(t0, Duration::from_secs(30 * 60))?);
        let window = registry
            .global_lease()
            .ok_or(TestError::Missing("global window"))?;
        assert_eq!(window.granted_at(), t0);
        assert!(!registry.has_global_approval_at(later(t0, hours(1))?));

        let t2 = later(t0, hours(2))?;
        registry.mark_global_approval_at(t2);
        assert!(registry.has_global_approval_at(t2));
        let renewed = registry
            .global_lease()
            .ok_or(TestError::Missing("renewed global window"))?;
        assert_eq!(renewed.granted_at(), t2);
        Ok(())
    }

    #[test]
    fn test_lease_remaining_uses_latest_covering_lease() -> TestResult {
        let registry = HostPermitSessionRegistry::default();
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        registry.mark_global_approval_at(later(t0, hours(2))?);

        // Beide decken ab; das spätere Ende (global, t0 + 10 h) zählt.
        assert_eq!(
            registry.lease_remaining_at("s1", later(t0, hours(3))?),
            Some(hours(7))
        );
        assert_eq!(
            registry.lease_remaining_at("foreign-session", later(t0, hours(3))?),
            Some(hours(7))
        );
        // Eigene Phase abgelaufen, globale noch aktiv.
        assert_eq!(
            registry.lease_remaining_at("s1", later(t0, hours(9))?),
            Some(hours(1))
        );
        assert_eq!(
            registry.lease_remaining_at("s1", later(t0, hours(10))?),
            None
        );
        Ok(())
    }

    #[test]
    fn test_remembered_lease_permit_is_not_returned_after_lease_expiry() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let registry = HostPermitSessionRegistry::default();
        let req = request("s1", "echo hi");
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);

        // Wie der Shell-Executor: Permit an das Fenster der laufenden Phase
        // binden und merken.
        let t7 = later(t0, hours(7))?;
        let window = registry
            .covering_lease_at("s1", t7)
            .ok_or(TestError::Missing("covering lease at t7"))?;
        let id = ledger
            .issue_for_lease_at(req.clone(), window, t7)
            .map_err(ctx("issue under an active lease failed"))?;
        registry.remember_permit_at(req.clone(), id, t7);
        assert_eq!(registry.lookup_permit_at(&req, t7), Some(id));

        let boundary = later(t0, hours(8))?;
        assert_eq!(
            registry.lookup_permit_at(&req, just_before(boundary)?),
            Some(id)
        );
        assert!(
            ledger
                .authorize_at(id, &req, just_before(boundary)?)
                .is_ok()
        );

        // Nach dem Ende der Phase: weder Registry noch Ledger tragen den
        // Antrag weiter; der Aufrufer muss erneut fragen.
        assert_eq!(registry.lookup_permit_at(&req, boundary), None);
        assert!(!registry.is_session_approved_at("s1", boundary));
        assert_eq!(
            ledger.authorize_at(id, &req, boundary),
            Err(ProcessPermitError::Expired)
        );
        Ok(())
    }

    #[test]
    fn test_remembered_permit_without_lease_is_not_lease_bound() -> TestResult {
        // Ohne Phase gemerkt (Einmalfreigabe): die Registry liefert den
        // Eintrag weiter, die Gültigkeit prüft allein der Ledger.
        let ledger = ProcessPermitLedger::default();
        let registry = HostPermitSessionRegistry::default();
        let req = request("s1", "echo once");
        let t0 = LeaseInstant::now();
        let id = issue(&ledger, &req)?;
        registry.remember_permit_at(req.clone(), id, t0);
        assert_eq!(
            registry.lookup_permit_at(&req, later(t0, hours(100))?),
            Some(id)
        );
        Ok(())
    }

    #[test]
    fn test_take_single_use_consumes_global_single_use_exactly_once_for_a_foreign_session() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.take_single_use("foreign-session"));
        registry.mark_global_single_use();
        assert!(
            registry.take_single_use("foreign-session"),
            "a global single-use approval must cover a session id that never requested it"
        );
        assert!(
            !registry.take_single_use("foreign-session"),
            "a second take for any session must not succeed"
        );
        assert!(
            !registry.take_single_use("another-foreign-session"),
            "the global single-use approval must be consumed exactly once across all sessions"
        );
    }

    #[test]
    fn test_take_single_use_prefers_session_own_single_use_over_global() {
        let registry = HostPermitSessionRegistry::default();
        registry.mark_single_use("s1".to_owned());
        registry.mark_global_single_use();

        assert!(registry.take_single_use("s1"));
        assert!(
            registry.has_global_single_use(),
            "consuming a session-own single-use approval must not spend the global one"
        );
        assert!(registry.take_single_use("s2"));
        assert!(!registry.has_global_single_use());
    }

    #[test]
    fn test_has_single_use_reflects_global_state_without_consuming() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.has_single_use("foreign-session"));
        registry.mark_global_single_use();
        assert!(registry.has_single_use("foreign-session"));
        assert!(
            registry.has_single_use("foreign-session"),
            "has_single_use must not consume the global entry"
        );
        assert!(registry.take_single_use("foreign-session"));
        assert!(!registry.has_single_use("foreign-session"));
    }

    #[test]
    fn test_has_global_single_use_before_and_after_mark() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.has_global_single_use());
        registry.mark_global_single_use();
        assert!(registry.has_global_single_use());
    }

    #[test]
    fn test_revoke_global_approval_removes_both_global_approval_and_global_single_use() {
        let registry = HostPermitSessionRegistry::default();
        registry.mark_global_approval();
        registry.mark_global_single_use();

        registry.revoke_global_approval();
        assert!(!registry.is_session_approved("foreign-session"));
        assert!(!registry.has_global_single_use());
        assert!(!registry.has_global_approval());

        // Zweiter Aufruf ist ein No-Op statt eines Fehlers oder einer Panik.
        registry.revoke_global_approval();
        assert!(!registry.is_session_approved("foreign-session"));
    }

    #[test]
    fn test_revoke_global_approval_leaves_an_independent_session_approval_untouched() {
        let registry = HostPermitSessionRegistry::default();
        registry.mark_session_approved("s1");
        registry.mark_global_approval();

        registry.revoke_global_approval();

        assert!(
            registry.is_session_approved("s1"),
            "revoking the global approval must not remove an independent session-own approval"
        );
        assert!(!registry.is_session_approved("foreign-session"));
    }

    // `base`, bei dem nur die Suspend-Uhr um `slept` weitergelaufen ist
    // (die monotone Uhr steht im Suspend still).
    fn after_suspend(base: LeaseInstant, slept: Duration) -> TestResult<LeaseInstant> {
        let since_boot = base
            .since_boot()
            .checked_add(slept)
            .ok_or(TestError::Missing("boot time after suspend"))?;
        Ok(LeaseInstant::from_parts(base.monotonic(), since_boot))
    }

    #[test]
    fn test_suspend_ends_session_and_global_leases() -> TestResult {
        // B22: genehmigt am Abend, über Nacht im Suspend. Monoton ist kaum
        // Zeit vergangen, nach Wanduhr mehr als 8 h: beide Phasen sind vorbei.
        let registry = HostPermitSessionRegistry::default();
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        registry.mark_global_approval_at(t0);
        let next_morning = after_suspend(later(t0, Duration::from_secs(60))?, hours(10))?;
        assert!(!registry.is_session_approved_at("s1", next_morning));
        assert!(!registry.has_global_approval_at(next_morning));
        assert!(!registry.is_session_approved_at("foreign-session", next_morning));
        assert_eq!(registry.covering_lease_at("s1", next_morning), None);
        assert_eq!(registry.lease_remaining_at("s1", next_morning), None);

        // Kurz vor der Grenze der Suspend-Uhr gilt die Phase noch, mit der
        // kleineren Restzeit beider Uhren.
        let almost = after_suspend(t0, hours(7))?;
        assert!(registry.is_session_approved_at("s1", almost));
        assert_eq!(registry.lease_remaining_at("s1", almost), Some(hours(1)));
        // Genau auf der Grenze: abgelaufen.
        assert!(!registry.is_session_approved_at("s1", after_suspend(t0, hours(8))?));
        Ok(())
    }

    #[test]
    fn test_suspend_drops_remembered_lease_permits() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let registry = HostPermitSessionRegistry::default();
        let req = request("s1", "echo hi");
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        let window = registry
            .covering_lease_at("s1", t0)
            .ok_or(TestError::Missing("covering lease"))?;
        let id = ledger
            .issue_for_lease_at(req.clone(), window, t0)
            .map_err(ctx("issue_for_lease_at failed"))?;
        registry.remember_permit_at(req.clone(), id, t0);
        let resumed = after_suspend(later(t0, hours(1))?, hours(8))?;
        assert_eq!(registry.lookup_permit_at(&req, resumed), None);
        assert_eq!(
            ledger.authorize_at(id, &req, resumed),
            Err(ProcessPermitError::Expired)
        );
        Ok(())
    }

    #[test]
    fn test_expired_lease_yields_no_window_so_the_caller_must_ask() -> TestResult {
        let ledger = ProcessPermitLedger::default();
        let registry = registry_with_ttl(hours(1))?;
        let req = request("s1", "echo late");
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        let window = registry
            .covering_lease_at("s1", t0)
            .ok_or(TestError::Missing("covering lease"))?;
        let t2 = later(t0, hours(2))?;
        // Abgelaufen: kein Fenster, keine Zustimmung, und auch ein noch
        // gehaltenes altes Fenster ergibt keinen neuen Permit.
        assert_eq!(registry.covering_lease_at("s1", t2), None);
        assert!(!registry.is_session_approved_at("s1", t2));
        assert_eq!(
            ledger.issue_for_lease_at(req, window, t2),
            Err(ProcessPermitError::Expired)
        );
        Ok(())
    }

    #[test]
    fn test_covering_lease_prefers_the_longer_remaining_window() -> TestResult {
        let registry = HostPermitSessionRegistry::default();
        let t0 = LeaseInstant::now();
        registry.mark_session_approved_at("s1", t0);
        let t2 = later(t0, hours(2))?;
        registry.mark_global_approval_at(t2);
        let global = registry
            .global_lease()
            .ok_or(TestError::Missing("global lease"))?;
        let own = registry
            .session_lease("s1")
            .ok_or(TestError::Missing("session lease"))?;
        assert_eq!(registry.covering_lease_at("s1", t2), Some(global));
        // Ohne die globale Phase bleibt die eigene.
        registry.revoke_global_approval();
        assert_eq!(registry.covering_lease_at("s1", t2), Some(own));
        assert_eq!(registry.covering_lease_at("s2", t2), None);
        Ok(())
    }
}
