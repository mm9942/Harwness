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
//! # Kein Zeitablauf (Nutzerentscheidung 2026-09-24)
//! Eine Sitzungs- oder prozessweite Freigabe (Host-Arbeitsphase) hat **keine**
//! Ablaufzeit: sie bleibt aktiv, bis der Nutzer sie selbst beendet — über
//! Strg+H in der TUI (`ChatApp::end_host_mode`) oder das getippte
//! `/sandbox-lease revoke`. Das Modell kann eine Phase weder beenden noch
//! verlängern. Einmal-Freigaben bleiben einmalig. Der Zustand ist rein
//! In-Memory; ein Prozessende beendet jede Phase ohnehin.
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

use crate::process_permit::{ProcessPermitId, ProcessPermitRequest};
use std::sync::Mutex;

/// Ein Eintrag: welcher exakte Antrag bereits einen ausgestellten Permit hat.
#[derive(Debug, Clone)]
struct RememberedPermit {
    request: ProcessPermitRequest,
    id: ProcessPermitId,
}

#[derive(Debug, Default)]
struct RegistryState {
    /// Sitzungen, deren Nutzer der lokalen UI einmalig für Host-Ausführung
    /// zugestimmt hat — ohne Ablaufzeitpunkt: der Eintrag bleibt, bis der
    /// Nutzer die Phase beendet ([`HostPermitSessionRegistry::forget_session`]/
    /// [`HostPermitSessionRegistry::revoke_session_approval`]).
    approved_sessions: Vec<String>,
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
    /// sie entfernt — kein Zeitablauf.
    global_approval: bool,
    /// Prozessweite Einmal-Freigabe: der nächste
    /// [`HostPermitSessionRegistry::take_single_use`]-Aufruf **jeder**
    /// Session-ID verbraucht sie, genau wie `single_use` es für eine
    /// einzelne Sitzung tut.
    global_single_use: bool,
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
#[derive(Debug, Default)]
pub struct HostPermitSessionRegistry {
    state: Mutex<RegistryState>,
}

impl HostPermitSessionRegistry {
    /// Merkt sich, dass die lokale UI Host-Ausführung für `session` erlaubt
    /// hat — bis der Nutzer die Phase selbst beendet (kein Zeitablauf).
    ///
    /// # Beschreibung
    /// Wird genau einmal pro Sitzung aufgerufen, ausgelöst durch den
    /// Bestätigungsdialog. Danach darf der Aufrufer für jeden neuen,
    /// abweichenden Befehl innerhalb derselben Sitzung ohne erneutes Fragen
    /// einen eigenen Permit über [`crate::ProcessPermitLedger::issue_after_local_approval`]
    /// ausstellen lassen — die Zustimmung des Menschen bezog sich auf die
    /// Sitzung, nicht auf einen einzelnen Befehlstext. Ein erneuter Aufruf für
    /// dieselbe `session` ist idempotent (kein zweiter Eintrag). Die Zustimmung
    /// endet ausschließlich über [`Self::forget_session`] bzw.
    /// [`Self::revoke_session_approval`] (Strg+H bzw. `/sandbox-lease revoke`)
    /// oder mit dem Prozess.
    ///
    /// # Argumente
    /// - `session` (`impl Into<String>`): die Sitzungs-ID, die zugestimmt hat.
    ///
    /// # Panics
    /// Nie: ein vergifteter [`Mutex`] (ein Aufrufer hat unter Halten der Sperre
    /// paniert) wird über `if let Ok(..)` übersprungen, statt selbst zu
    /// paniken — die Zustimmung geht in diesem seltenen Fall verloren, aber
    /// der Aufrufer stürzt nicht ab.
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
    /// assert!(registry.is_session_approved("session-1"));
    /// ```
    pub fn mark_session_approved(&self, session: impl Into<String>) {
        let session = session.into();
        if let Ok(mut state) = self.state.lock() {
            if !state.approved_sessions.contains(&session) {
                state.approved_sessions.push(session);
            }
        }
    }

    /// `true`, wenn `session` eine aktive lokale Host-Zustimmung hat.
    ///
    /// # Beschreibung
    /// Rein lesend; es gibt keinen Zeitablauf, eine Zustimmung bleibt bis zum
    /// Widerruf durch den Nutzer bestehen. Liefert zusätzlich `true`, wenn eine
    /// [`Self::mark_global_approval`]-Freigabe aktiv ist — diese gilt
    /// prozessweit, also auch für eine `session`, die nie selbst über
    /// [`Self::mark_session_approved`] zugestimmt hat (Root-Session und jede
    /// Kind-Session teilen sich dieselbe Registry-Instanz).
    ///
    /// # Argumente
    /// - `session` (`&str`): die zu prüfende Sitzungs-ID.
    ///
    /// # Returns
    /// `true`, wenn für `session` eine nicht widerrufene Zustimmung aus
    /// [`Self::mark_session_approved`] vorliegt, **oder** eine nicht
    /// widerrufene prozessweite Freigabe aus [`Self::mark_global_approval`]
    /// aktiv ist; `false` für eine unbekannte, widerrufene oder noch nie
    /// zugestimmte Sitzung ohne aktive globale Freigabe — und ebenso `false`,
    /// wenn der interne [`Mutex`] vergiftet ist (fail-closed statt Panik).
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
    /// assert!(!registry.is_session_approved("unknown-session"));
    /// ```
    #[must_use]
    pub fn is_session_approved(&self, session: &str) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        state.global_approval
            || state
                .approved_sessions
                .iter()
                .any(|existing| existing == session)
    }

    /// Liefert die bereits gemerkte Permit-Kennung für einen exakt
    /// identischen Antrag, falls vorhanden.
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
    /// `request` aufgerufen wurde; sonst `None` — auch bei vergiftetem
    /// [`Mutex`] (fail-closed).
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
        let state = self.state.lock().ok()?;
        state
            .remembered
            .iter()
            .find(|entry| &entry.request == request)
            .map(|entry| entry.id)
    }

    /// Merkt sich eine frisch ausgestellte Permit-Kennung für ihren exakten
    /// Antrag, damit derselbe Antrag später ohne erneute Ausstellung
    /// autorisiert werden kann.
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
    ///     HostApprovalScope, HostPermitSessionRegistry, ProcessEnvironment, ProcessPermitLedger,
    ///     request_for_workspace,
    /// };
    /// use std::path::Path;
    ///
    /// let ledger = ProcessPermitLedger::default();
    /// let registry = HostPermitSessionRegistry::default();
    /// let request = request_for_workspace(
    ///     "session-1",
    ///     "host-process-worker@1",
    ///     "echo hi",
    ///     Path::new("/workspace"),
    ///     ProcessEnvironment::LocalHost,
    /// );
    /// let id = ledger
    ///     .issue_after_local_approval(request.clone(), HostApprovalScope::SessionLease, None)
    ///     .expect("issuing a valid request must succeed");
    /// registry.remember_permit(request.clone(), id);
    /// assert_eq!(registry.lookup_permit(&request), Some(id));
    /// ```
    pub fn remember_permit(&self, request: ProcessPermitRequest, id: ProcessPermitId) {
        if let Ok(mut state) = self.state.lock() {
            state.remembered.retain(|entry| entry.request != request);
            state.remembered.push(RememberedPermit { request, id });
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
            .retain(|existing| existing != session);
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
        if let Ok(mut state) = self.state.lock() {
            if !state.single_use.iter().any(|existing| existing == &session) {
                state.single_use.push(session);
            }
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
                .retain(|existing| existing != session);
            state.single_use.retain(|existing| existing != session);
        }
    }

    /// Merkt sich eine prozessweite Host-Ausführungsfreigabe („volle
    /// Sandbox-Deaktivierung“, Nutzerwunsch 2026-09-21): gilt bis zum
    /// Widerruf durch den Nutzer ([`Self::revoke_global_approval`]) für
    /// **jede** Session-ID desselben `harw`-Prozesses — die
    /// Root-Session und jede Kind-Session, die dieselbe Registry-Instanz
    /// teilt (siehe `harw_registry_defaults::profile::build_shell_provider`,
    /// das Ledger/Registry/Fragekanal jetzt an jeden `ShellToolProvider`
    /// hängt, nicht nur an einen mit `SandboxProfile::Host`).
    ///
    /// # Beschreibung
    /// Idempotent; es gibt keinen Zeitablauf (Nutzerentscheidung
    /// 2026-09-24): die Freigabe endet ausschließlich über
    /// [`Self::revoke_global_approval`] (Strg+H bzw. `/sandbox-lease revoke`)
    /// oder mit dem Prozess.
    ///
    /// # Panics
    /// Nie: ein vergifteter [`Mutex`] wird über `if let Ok(..)` übersprungen,
    /// statt selbst zu paniken — die Freigabe geht in diesem seltenen Fall
    /// verloren, aber der Aufrufer stürzt nicht ab.
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
    /// assert!(registry.is_session_approved("any-session-id"));
    /// ```
    pub fn mark_global_approval(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.global_approval = true;
        }
    }

    /// `true`, solange die prozessweite Freigabe aus
    /// [`Self::mark_global_approval`] aktiv ist (nicht widerrufen).
    ///
    /// # Returns
    /// `true` nach [`Self::mark_global_approval`] und vor
    /// [`Self::revoke_global_approval`]; sonst `false` — auch bei vergiftetem
    /// [`Mutex`] (fail-closed).
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
    /// assert!(!registry.has_global_approval());
    /// registry.mark_global_approval();
    /// assert!(registry.has_global_approval());
    /// ```
    #[must_use]
    pub fn has_global_approval(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.global_approval)
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
            state.global_approval = false;
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
    use crate::test_support::{TestError, TestResult};
    use std::path::Path;
    use std::time::Duration;

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
    fn test_session_approval_does_not_expire_over_time() {
        // Nutzerentscheidung 2026-09-24: kein Zeitablauf — nur der Nutzer
        // beendet eine Host-Arbeitsphase.
        let registry = HostPermitSessionRegistry::default();
        registry.mark_session_approved("s1");
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            registry.is_session_approved("s1"),
            "a session approval must stay active until the user revokes it"
        );
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
    fn test_global_approval_does_not_expire_over_time() {
        let registry = HostPermitSessionRegistry::default();
        registry.mark_global_approval();
        std::thread::sleep(Duration::from_millis(30));
        assert!(registry.has_global_approval());
        assert!(
            registry.is_session_approved("foreign-session"),
            "a global approval must stay active until the user revokes it"
        );
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
}
