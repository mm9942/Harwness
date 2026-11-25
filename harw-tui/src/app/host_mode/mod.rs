//! ChatApp methods: host mode.

use super::*;

impl ChatApp {
    /// `true`, solange diese Sitzung eine laufende Host-Arbeitsphase
    /// ([`HostPermitVariant::SessionLease`]) hat.
    ///
    /// # Beschreibung
    /// Fragt direkt [`harw_sandbox::HostPermitSessionRegistry::is_session_approved`]
    /// über die Runtime-Montage ab, statt einen eigenen Merker zu pflegen — ein
    /// Beenden per Strg+H ([`Self::end_host_mode`]) oder per getipptem
    /// `/sandbox-lease revoke` wirkt dadurch ohne einen zweiten Wahrheitsort
    /// sofort auch hier. Eine Phase kann vom Modell weder beendet noch
    /// verlängert werden und läuft nach `[sandbox] host_lease_ttl_secs`
    /// (Vorgabe 8 h) ab (PL-100 Phase 4, B22; ersetzt die
    /// Nutzerentscheidung 2026-09-24 „kein Zeitablauf“): danach liefert diese
    /// Methode `false`, auch bevor [`Self::poll_host_lease_expiry`] aufräumt.
    /// `false`, wenn keine Runtime-Montage vorliegt (z. B. in reinen
    /// Renderer-Tests).
    #[must_use]
    pub(crate) fn host_mode_active(&self) -> bool {
        self.runtime.as_ref().is_some_and(|runtime| {
            runtime
                .host_permit_session_registry()
                .is_session_approved(self.session_id.to_string().as_str())
        })
    }

    /// Beendet eine laufende Host-Arbeitsphase dieser Sitzung sofort (Ctrl+H,
    /// siehe [`handle_key`]) und widerruft alle dafür ausgestellten Permits;
    /// räumt ebenso eine bereits abgelaufene Phase auf.
    ///
    /// # Beschreibung
    /// Ruft [`harw_sandbox::HostPermitSessionRegistry::forget_session`] (löscht
    /// die Sitzungszustimmung und alle gemerkten Permit-Zuordnungen) und
    /// zusätzlich [`harw_sandbox::ProcessPermitLedger::revoke_session`] (entzieht
    /// auch bereits ausgestellte, aber noch nicht gemerkte Permits derselben
    /// Sitzung) auf. Ohne die zweite Erweiterung könnte ein bereits
    /// ausgestellter, aber dem Renderer nie gemeldeter Permit die Isolation
    /// überdauern. Strg+H ist eine ausdrückliche Nutzerentscheidung: eine
    /// offene prozessweite Einmalfreigabe endet mit (siehe
    /// [`Self::clear_host_lease_in`]).
    ///
    /// Seit PL-100 Phase 4 (B22) läuft eine Phase ab. Ein abgelaufenes
    /// Fenster bleibt in der Registry gespeichert
    /// ([`harw_sandbox::HostPermitSessionRegistry::session_lease`] bzw.
    /// [`harw_sandbox::HostPermitSessionRegistry::global_lease`]), bis jemand
    /// es entfernt. Diese Methode räumt deshalb auf, sobald ein solches
    /// Fenster gespeichert ist, aktiv oder abgelaufen (vorher kehrte sie nach
    /// dem Ablauf früh zurück und ließ den Zustand liegen). Die Systemzeile
    /// lautet „beendet“ für eine aktive und „abgelaufen“ für eine bereits
    /// abgelaufene Phase.
    ///
    /// # Rückgabe
    /// `true`, wenn eine aktive oder abgelaufene Phase aufgeräumt wurde (und
    /// damit ein Redraw sowie eine Systemzeile angebracht sind); `false`, wenn
    /// kein Fenster gespeichert war oder keine Runtime-Montage vorliegt.
    pub(crate) fn end_host_mode(&mut self) -> bool {
        let Some(runtime) = self.runtime.clone() else {
            return false;
        };
        let session = self.session_id.to_string();
        let Some(was_active) = Self::clear_host_lease_in(
            runtime.host_permit_session_registry(),
            runtime.host_permit_ledger(),
            &session,
            false,
        ) else {
            return false;
        };
        self.announce_host_lease_end(was_active);
        true
    }

    /// Räumt die Host-Arbeitsphase von `session` in `registry` und `ledger`
    /// auf: gemeinsamer Kern von [`Self::end_host_mode`] (Strg+H) und
    /// [`Self::poll_host_lease_expiry`] (Ablauf), ohne Runtime-Montage
    /// testbar.
    ///
    /// # Beschreibung
    /// Ist weder ein sitzungseigenes noch ein prozessweites Fenster
    /// gespeichert (aktiv oder abgelaufen), ändert die Funktion nichts.
    /// Sonst:
    /// - [`harw_sandbox::HostPermitSessionRegistry::forget_session`] entfernt
    ///   das Fenster der Sitzung und ihre gemerkten Permits;
    /// - [`harw_sandbox::HostPermitSessionRegistry::revoke_global_approval`]
    ///   beendet die prozessweite Phase (Runde 5, Teil N: sie gilt für die
    ///   Root-Sitzung und jedes Kind; sonst meldete die Zeile „beendet“,
    ///   während Kinder weiter auf dem Host liefen);
    /// - [`harw_sandbox::ProcessPermitLedger::revoke_session`] widerruft die
    ///   Ledger-Permits der Sitzung.
    ///
    /// [`harw_sandbox::HostPermitSessionRegistry::revoke_global_approval_taking_ids`]
    /// beendet die globale Phase und liefert die unter ihr gemerkten Permits
    /// (auch die von Kind-Sitzungen), die einzeln im Ledger widerrufen werden.
    /// Es löscht auch eine offene prozessweite Einmalfreigabe. Mit `keep_pending_single_use` (automatisches
    /// Aufräumen nach dem Ablauf) wird sie danach wieder eingetragen: sie ist
    /// eine eigene Nutzerentscheidung (z. B. „einmalig“ auf ein
    /// `/sandbox-lease request` nach dem Ablauf) und darf nicht still
    /// verschwinden. Strg+H übergibt `false` und beendet sie mit.
    ///
    /// # Argumente
    /// - `registry`: die Sitzungs-Registry der Montage.
    /// - `ledger`: der Permit-Ledger der Montage.
    /// - `session`: die Root-Sitzung dieser TUI.
    /// - `keep_pending_single_use`: offene prozessweite Einmalfreigabe
    ///   erhalten (`true`) oder mit beenden (`false`).
    ///
    /// # Rückgabe
    /// - `None`: kein Fenster gespeichert, nichts geändert.
    /// - `Some(true)`: eine aktive Phase deckte die Sitzung ab und wurde
    ///   beendet.
    /// - `Some(false)`: nur ein bereits abgelaufenes Fenster war gespeichert
    ///   und wurde entfernt.
    ///
    /// # Concurrency
    /// Jede Registry- und Ledger-Methode sperrt kurz ihren eigenen `Mutex`;
    /// die Folge ist nicht atomar. Erteilt jemand genau dazwischen eine neue
    /// Phase, endet sie mit (fail-closed).
    pub(super) fn clear_host_lease_in(
        registry: &harw_sandbox::HostPermitSessionRegistry,
        ledger: &harw_sandbox::ProcessPermitLedger,
        session: &str,
        keep_pending_single_use: bool,
    ) -> Option<bool> {
        let stored = registry.session_lease(session).is_some() || registry.global_lease().is_some();
        if !stored {
            return None;
        }
        let was_active = registry.is_session_approved(session);
        let restore_single_use = keep_pending_single_use && registry.has_global_single_use();
        // Die zurückgegebenen Kennungen widerruft `revoke_session` unten
        // ohnehin (alle Permits dieser Sitzung), daher nicht einzeln.
        let _ = registry.forget_session(session);
        // Auch die unter der globalen Phase gemerkten Permits von
        // Kind-Sitzungen enden im Ledger, nicht erst beim nächsten Lookup
        // (Owner-Review 4505015300). Ein vergifteter Ledger ist hier nicht
        // behebbar; die Registry-Einträge sind ohnehin entfernt.
        for id in registry.revoke_global_approval_taking_ids() {
            let _ = ledger.revoke(id);
        }
        if restore_single_use {
            registry.mark_global_single_use();
        }
        // Ein vergifteter Ledger ist hier nicht behebbar; seine Lease-Permits
        // sind ohnehin an das Fenster gebunden und enden spätestens mit ihm.
        let _ = ledger.revoke_session(session);
        Some(was_active)
    }

    // Systemzeile „beendet“ (aktive Phase) bzw. „abgelaufen“ und
    // Vormerkung der Terminal-Reassertion nach dem Aufräumen einer Phase.
    pub(super) fn announce_host_lease_end(&mut self, was_active: bool) {
        let notice = if was_active {
            tr_for(
                self.language,
                "app_notices.host_ended",
                "Host mode ended — isolation is active again.",
            )
        } else {
            tr_for(
                self.language,
                "app_notices.host_lease_expired",
                "Host work phase expired — isolation is active again.",
            )
        };
        self.push_line(Role::System, notice);
        // Register „CSI-Sicherheitsnetz und Paste-Platzhalter", Punkt 7: die
        // beendete Host-Arbeitsphase kann Terminal-Modi (Raw-Mode/Alternate-Screen/Bracketed-
        // Paste/Maus-Capture) beschädigt zurückgelassen haben — der Aufrufer
        // mit Zugriff auf den `TerminalGuard` reasserted sie best-effort.
        self.request_terminal_reassert();
    }

    /// Höchster Abstand zwischen zwei Ablaufprüfungen einer Host-Arbeitsphase
    /// ([`Self::host_lease_poll_delay`]).
    ///
    /// # Beschreibung
    /// `tokio`-Timer laufen auf der monotonen Uhr, die im Suspend stillsteht;
    /// die Registry misst dagegen auch Suspend mit (`CLOCK_BOOTTIME`). Die
    /// Obergrenze sorgt dafür, dass der Hinweis nach einem Aufwachen
    /// spätestens nach diesem Abstand erscheint. Die Autorisierung selbst
    /// hängt nie an dieser Prüfung: eine abgelaufene Phase wirkt sofort nicht
    /// mehr.
    pub(crate) const HOST_LEASE_POLL_INTERVAL: Duration = Duration::from_secs(30);

    /// Wartezeit bis zur nächsten Ablaufprüfung der Host-Arbeitsphase dieser
    /// Sitzung, für den Timer-Zweig der Ereignisschleife ([`run_loop`]).
    ///
    /// # Rückgabe
    /// Siehe [`Self::host_lease_poll_delay_in`]; `None` auch ohne
    /// Runtime-Montage.
    ///
    /// # Concurrency
    /// Rein lesend; sperrt kurz die Registry der Runtime-Montage.
    #[must_use]
    pub(crate) fn host_lease_poll_delay(&self) -> Option<Duration> {
        let runtime = self.runtime.as_ref()?;
        Self::host_lease_poll_delay_in(
            runtime.host_permit_session_registry(),
            &self.session_id.to_string(),
        )
    }

    /// Wartezeit bis zur nächsten Ablaufprüfung für `session` in `registry`
    /// (ohne Runtime-Montage testbar).
    ///
    /// # Rückgabe
    /// - `None`: kein Fenster gespeichert (keine Phase, oder bereits
    ///   aufgeräumt); kein Timer nötig.
    /// - `Some(d)`: die Restlaufzeit der abdeckenden Phase, höchstens
    ///   [`Self::HOST_LEASE_POLL_INTERVAL`]; `Duration::ZERO`, wenn das
    ///   gespeicherte Fenster bereits abgelaufen ist (sofort prüfen).
    pub(super) fn host_lease_poll_delay_in(
        registry: &harw_sandbox::HostPermitSessionRegistry,
        session: &str,
    ) -> Option<Duration> {
        if registry.session_lease(session).is_none() && registry.global_lease().is_none() {
            return None;
        }
        Some(
            registry
                .lease_remaining(session)
                .unwrap_or(Duration::ZERO)
                .min(Self::HOST_LEASE_POLL_INTERVAL),
        )
    }

    /// `true`, wenn für `session` in `registry` ein Fenster gespeichert ist,
    /// aber keine aktive Phase die Sitzung mehr abdeckt (abgelaufen, noch
    /// nicht aufgeräumt).
    pub(super) fn host_lease_expired_in(
        registry: &harw_sandbox::HostPermitSessionRegistry,
        session: &str,
    ) -> bool {
        let stored = registry.session_lease(session).is_some() || registry.global_lease().is_some();
        stored && registry.covering_lease(session).is_none()
    }

    /// Prüft, ob die Host-Arbeitsphase dieser Sitzung abgelaufen ist, und
    /// räumt sie dann mit einer Systemzeile auf (PL-100 Phase 4, B22).
    ///
    /// # Beschreibung
    /// Deckt keine aktive Phase die Sitzung mehr ab
    /// ([`harw_sandbox::HostPermitSessionRegistry::covering_lease`] ist
    /// `None`), ist aber noch ein Fenster gespeichert, entfernt diese Methode
    /// den gespeicherten Zustand ([`Self::clear_host_lease_in`]) und meldet
    /// genau einmal „abgelaufen“ (danach ist nichts mehr gespeichert). Eine
    /// offene prozessweite Einmalfreigabe bleibt dabei erhalten. Der
    /// Statuszeilen-Marker ([`Self::host_mode_active`]) verschwindet schon mit
    /// dem Ablauf selbst. Aufrufer: der Anfang jeder Runde von [`run_loop`],
    /// geweckt über den Timer-Zweig nach [`Self::host_lease_poll_delay`]; bei
    /// `true` neu zeichnen und die Terminal-Modi reassertieren.
    ///
    /// Erteilt der Nutzer genau zwischen Prüfung und Aufräumen eine neue
    /// Phase, beendet das Aufräumen sie wieder und meldet „beendet“
    /// (fail-closed und sichtbar; die Registry hat kein atomares
    /// „nur abgelaufene entfernen“).
    ///
    /// # Rückgabe
    /// `true`, wenn aufgeräumt wurde (Redraw nötig); sonst `false`.
    pub(crate) fn poll_host_lease_expiry(&mut self) -> bool {
        let Some(runtime) = self.runtime.clone() else {
            return false;
        };
        let session = self.session_id.to_string();
        let registry = runtime.host_permit_session_registry();
        if !Self::host_lease_expired_in(registry, &session) {
            return false;
        }
        let Some(was_active) =
            Self::clear_host_lease_in(registry, runtime.host_permit_ledger(), &session, true)
        else {
            return false;
        };
        self.announce_host_lease_end(was_active);
        true
    }

    /// Merkt vor, dass Terminal-Modi best-effort reasserted werden sollen
    /// (siehe [`Self::needs_terminal_reassert`]-Dokumentation).
    pub(crate) fn request_terminal_reassert(&mut self) {
        self.needs_terminal_reassert = true;
    }

    /// Konsumiert die Vormerkung aus [`Self::request_terminal_reassert`].
    ///
    /// # Rückgabe
    /// `true` genau einmal pro Vormerkung — der Aufrufer ruft danach
    /// [`TerminalGuard::reassert_terminal_modes`] auf.
    #[must_use]
    pub(crate) fn take_needs_terminal_reassert(&mut self) -> bool {
        std::mem::take(&mut self.needs_terminal_reassert)
    }
}
