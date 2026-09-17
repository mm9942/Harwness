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
//! # Verantwortungsgrenze
//! Diese Datei ändert nichts an `process_permit.rs`: sie hält ausschließlich
//! Zusatzzustand für den Aufrufer (den Shell-Executor und die lokale
//! Bestätigungs-UI), damit beide denselben "hat diese Sitzung schon
//! zugestimmt"-Zustand sehen, ohne die Ledger-Kernlogik zu erweitern.
//!
//! # Nebenläufigkeit
//! [`HostPermitSessionRegistry`] ist `Send + Sync`; der gesamte Zustand liegt
//! hinter einem einzigen [`Mutex`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::process_permit::{ProcessPermitId, ProcessPermitRequest};

/// Ein Eintrag: welcher exakte Antrag bereits einen ausgestellten Permit hat.
#[derive(Debug, Clone)]
struct RememberedPermit {
    request: ProcessPermitRequest,
    id: ProcessPermitId,
}

#[derive(Debug, Default)]
struct RegistryState {
    /// Sitzungen, deren Nutzer der lokalen UI einmalig für Host-Ausführung
    /// zugestimmt hat, mit Ablaufzeitpunkt (Sitzungsende / Lease-Ende).
    approved_sessions: Vec<(String, Instant)>,
    /// Bereits ausgestellte Permits, um denselben Antrag ohne erneute
    /// `issue_after_local_approval`-Ausstellung wiederzuverwenden.
    remembered: Vec<RememberedPermit>,
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
    /// Merkt sich, dass die lokale UI Host-Ausführung für `session` bis
    /// `ttl` ab jetzt erlaubt hat.
    ///
    /// # Beschreibung
    /// Wird genau einmal pro Sitzung aufgerufen, ausgelöst durch den
    /// Bestätigungsdialog. Danach darf der Aufrufer für jeden neuen,
    /// abweichenden Befehl innerhalb derselben Sitzung ohne erneutes Fragen
    /// einen eigenen Permit über [`crate::ProcessPermitLedger::issue_after_local_approval`]
    /// ausstellen lassen — die Zustimmung des Menschen bezog sich auf die
    /// Sitzung, nicht auf einen einzelnen Befehlstext. Ein erneuter Aufruf für
    /// dieselbe `session` ersetzt die zuvor gemerkte Ablaufzeit vollständig
    /// (kein Verlängern über ein Maximum, kein Vereinigen zweier Fristen).
    ///
    /// # Argumente
    /// - `session` (`impl Into<String>`): die Sitzungs-ID, die zugestimmt hat.
    /// - `ttl` (`Duration`): wie lange die Zustimmung ab jetzt gilt, bevor
    ///   [`Self::is_session_approved`] wieder `false` liefert.
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
    /// use std::time::Duration;
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_session_approved("session-1", Duration::from_secs(60));
    /// assert!(registry.is_session_approved("session-1"));
    /// ```
    pub fn mark_session_approved(&self, session: impl Into<String>, ttl: Duration) {
        let session = session.into();
        let expires_at = Instant::now() + ttl;
        if let Ok(mut state) = self.state.lock() {
            state.approved_sessions.retain(|(existing, _)| existing != &session);
            state.approved_sessions.push((session, expires_at));
        }
    }

    /// `true`, wenn `session` noch eine gültige lokale Host-Zustimmung hat.
    ///
    /// # Beschreibung
    /// Räumt bei jedem Aufruf beiläufig alle bereits abgelaufenen
    /// Zustimmungen aus dem inneren Zustand (nicht nur die von `session`) —
    /// es gibt keinen separaten Hintergrund-Aufräumer, jeder Abfrageaufruf
    /// erledigt das selbst.
    ///
    /// # Argumente
    /// - `session` (`&str`): die zu prüfende Sitzungs-ID.
    ///
    /// # Returns
    /// `true`, wenn für `session` eine noch nicht abgelaufene Zustimmung aus
    /// [`Self::mark_session_approved`] vorliegt; `false` für eine unbekannte,
    /// abgelaufene oder noch nie zugestimmte Sitzung — und ebenso `false`,
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
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let now = Instant::now();
        state.approved_sessions.retain(|(_, expires_at)| *expires_at > now);
        state
            .approved_sessions
            .iter()
            .any(|(existing, _)| existing == session)
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
    /// use std::time::Duration;
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
    ///     .issue_after_local_approval(request.clone(), HostApprovalScope::SessionLease, Duration::from_secs(60))
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
    /// use std::time::Duration;
    /// use harw_sandbox::HostPermitSessionRegistry;
    ///
    /// let registry = HostPermitSessionRegistry::default();
    /// registry.mark_session_approved("session-1", Duration::from_secs(60));
    /// let removed = registry.forget_session("session-1");
    /// assert!(removed.is_empty(), "keine Permits waren gemerkt");
    /// assert!(!registry.is_session_approved("session-1"));
    /// ```
    pub fn forget_session(&self, session: &str) -> Vec<ProcessPermitId> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        state.approved_sessions.retain(|(existing, _)| existing != session);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_permit::{
        HostApprovalScope, ProcessEnvironment, ProcessPermitLedger, request_for_workspace,
    };
    use std::path::Path;

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
    fn issue(ledger: &ProcessPermitLedger, request: &ProcessPermitRequest) -> ProcessPermitId {
        ledger
            .issue_after_local_approval(
                request.clone(),
                HostApprovalScope::SessionLease,
                Duration::from_secs(60),
            )
            .expect("issuing a valid host request must succeed")
    }

    #[test]
    fn test_is_session_approved_before_and_after_mark_session_approved() {
        let registry = HostPermitSessionRegistry::default();
        assert!(!registry.is_session_approved("s1"));
        registry.mark_session_approved("s1", Duration::from_secs(60));
        assert!(registry.is_session_approved("s1"));
        assert!(!registry.is_session_approved("s2"));
    }

    #[test]
    fn test_is_session_approved_after_ttl_elapses_returns_false() {
        let registry = HostPermitSessionRegistry::default();
        registry.mark_session_approved("s1", Duration::from_millis(20));
        assert!(registry.is_session_approved("s1"));

        std::thread::sleep(Duration::from_millis(60));

        assert!(
            !registry.is_session_approved("s1"),
            "an expired lease must no longer count as approved"
        );
    }

    #[test]
    fn test_mark_session_approved_replaces_an_existing_approval() {
        // A second `mark_session_approved` for the same session must not
        // accumulate a stale short-lived entry alongside a fresh long-lived
        // one; the state carries exactly one expiry per session.
        let registry = HostPermitSessionRegistry::default();
        registry.mark_session_approved("s1", Duration::from_millis(20));
        registry.mark_session_approved("s1", Duration::from_secs(60));

        std::thread::sleep(Duration::from_millis(60));

        assert!(
            registry.is_session_approved("s1"),
            "the later, longer-lived approval must win over the earlier short one"
        );
    }

    #[test]
    fn test_remembered_permit_is_found_only_for_identical_request() {
        let ledger = ProcessPermitLedger::default();
        let registry = HostPermitSessionRegistry::default();
        let req = request("s1", "echo hi");
        assert_eq!(registry.lookup_permit(&req), None);
        let id = issue(&ledger, &req);
        registry.remember_permit(req.clone(), id);
        assert_eq!(registry.lookup_permit(&req), Some(id));

        let other = request("s1", "echo bye");
        assert_eq!(registry.lookup_permit(&other), None);
    }

    #[test]
    fn test_forget_session_clears_approval_and_returns_removed_ids() {
        let ledger = ProcessPermitLedger::default();
        let registry = HostPermitSessionRegistry::default();
        registry.mark_session_approved("s1", Duration::from_secs(60));
        let req = request("s1", "echo hi");
        let id = issue(&ledger, &req);
        registry.remember_permit(req.clone(), id);

        let removed = registry.forget_session("s1");
        assert_eq!(removed, vec![id]);
        assert!(!registry.is_session_approved("s1"));
        assert_eq!(registry.lookup_permit(&req), None);
    }
}
