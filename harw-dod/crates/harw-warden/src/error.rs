//! Fehlertyp dieses Binaries: nur die Startpfade, die den Prozess wirklich
//! beenden dürfen, plus der eine Laufzeitpfad, der ihn ohne Bedienfähigkeit
//! zurücklässt.
//!
//! # Verantwortungsbereich
//! [`WardenBinError`] ist der eine Fehlertyp von `harw-warden` im Sinne von
//! Contract-Master §H.1 („ein Fehlertyp je Crate"). Er deckt diese Quellen
//! ab: die systemd-Socket-Aktivierung ([`crate::systemd`]), das Laden von
//! Policy, Schlüsseln und Zustand ([`WardenBinError::Setup`], PL-100 WP 3.1),
//! die Auflösung der Escalator-uid ([`crate::peer`]), die harte
//! Landlock-Durchsetzung ([`crate::landlock`]), den seccomp-Filter
//! ([`crate::seccomp`], PL-100 O10), den Start des Hold-Timers
//! ([`crate::timer`]) und das endgültige Ende der IPC-Annahmeschleife
//! ([`crate::ipc`]). Nachrichten-lokale, wiederholbare
//! Fehler eines einzelnen Verbindungsversuchs gehören **nicht** hierher —
//! dafür ist [`crate::ipc::IpcError`] zuständig (dieselbe Aufteilung wie bei
//! `harw-sentinel`: `SentinelBinError` für den Prozess, `ipc::IpcError` für
//! eine einzelne Verbindung).
//!
//! # Warum es hier keine Degradation gibt
//! Jede Variante dieses Typs ist ein harter Startfehler oder das Ende des
//! einzigen Dienstes, den dieser Prozess anbietet. Für einen privilegierten
//! Durchsetzer gibt es keinen sinnvollen Teilbetrieb: ein Warden ohne
//! systemd-Socket kann keine Anfrage empfangen, ein Warden ohne
//! durchgesetztes Landlock stünde mit vollem Dateisystemzugriff da (siehe
//! `crate::landlock`-Moduldoku für die Asymmetrie gegenüber `harw-sentinel`),
//! und ein Warden, dessen Annahmeschleife endgültig aufgibt, kann nichts
//! mehr durchsetzen.
//!
//! # Kommandozeilenfehler gehören nicht hierher
//! `clap::Error` wird in `main` direkt behandelt, ohne Umweg über diesen Typ
//! — dasselbe Muster wie `harw-sentinel`/`harw-probe-fs`.
//!
//! # Nebenläufigkeit
//! `WardenBinError` ist `Send + Sync`, weil jedes seiner Felder es ist. Kein
//! internes Locking, keine geteilten Ressourcen.
//!
//! # Examples
//! ```rust,ignore
//! // `harw-warden` hat kein `lib`-Target (wie `harw-sentinel`/
//! // `harw-probe-fs`) — dieses Beispiel ist deshalb `ignore`, nicht
//! // ausführbar über `cargo test --doc` (siehe `crate::error`-Tests für die
//! // tatsächlich geprüfte Fassung).
//! use crate::error::WardenBinError;
//!
//! fn describe(err: &WardenBinError) -> String {
//!     err.to_string()
//! }
//! ```

use harw_macros::HarwError;

/// Fehler dieses Binaries, die den Prozess mit einem Fehlschlag beenden.
///
/// # Description
/// Siehe Moduldoku für die Einteilung der Varianten. `#[derive(HarwError)]`
/// erzeugt `Display`, `std::error::Error` (mit `source()` für
/// [`Self::ListenFdsAcquisitionFailed`]) und den Typalias
/// [`WardenBinResult`] — **kein** `Debug` (siehe `impl Debug` unterhalb).
#[derive(HarwError)]
pub enum WardenBinError {
    /// `LISTEN_PID` ist nicht gesetzt — dieser Prozess wurde nicht über
    /// systemd-Socket-Aktivierung gestartet.
    #[msg(
        "LISTEN_PID ist nicht gesetzt; dieses Binary muss über systemd-Socket-Aktivierung gestartet werden"
    )]
    ListenPidMissing,

    /// `LISTEN_PID` enthält keinen gültigen Prozess-Bezeichner.
    #[msg("LISTEN_PID enthält keinen gültigen Prozess-Bezeichner")]
    ListenPidMalformed,

    /// `LISTEN_PID` zeigt auf einen anderen Prozess als diesen — die
    /// übergebenen Deskriptoren gehören nicht diesem Prozess (siehe
    /// `crate::systemd`-Moduldoku für die Begründung, warum das ein
    /// eigenständig geprüfter Fall ist, nicht nur eine Spielart von
    /// [`Self::ListenPidMissing`]).
    #[msg(
        "LISTEN_PID zeigt auf einen anderen Prozess; die übergebenen Deskriptoren gehören nicht diesem Prozess"
    )]
    ListenPidForeign,

    /// `LISTEN_FDS` ist nicht gesetzt — kein Deskriptor wurde übergeben.
    #[msg("LISTEN_FDS ist nicht gesetzt; kein systemd-Socket wurde übergeben")]
    ListenFdsMissing,

    /// `LISTEN_FDS` enthält keine gültige Deskriptor-Anzahl.
    #[msg("LISTEN_FDS enthält keine gültige Deskriptor-Anzahl")]
    ListenFdsMalformed,

    /// Es wurde nicht genau ein Deskriptor übergeben.
    ///
    /// # Arguments
    /// - `actual` (`usize`): die tatsächlich deklarierte oder tatsächlich
    ///   erhaltene Anzahl.
    #[msg(
        "erwartete genau einen von systemd übergebenen Deskriptor, tatsächlich waren es {actual}"
    )]
    UnexpectedListenFdCount {
        /// Die tatsächliche Anzahl.
        actual: usize,
    },

    /// Das Einlesen der bereits validierten Aktivierungsumgebung über
    /// `sd_listen_fds::get()` ist trotzdem fehlgeschlagen (nur erreichbar,
    /// wenn sich die Umgebung zwischen der eigenen Vorprüfung in
    /// `crate::systemd` und diesem Aufruf geändert hat).
    ///
    /// # Arguments
    /// - `0` (`sd_listen_fds::Error`): die zugrunde liegende Ursache.
    #[msg("das Einlesen der systemd-Socket-Aktivierungsumgebung ist fehlgeschlagen: {0}")]
    #[from]
    ListenFdsAcquisitionFailed(sd_listen_fds::Error),

    /// Landlock hat den Zugriff auf das cgroup-Wurzelverzeichnis nicht
    /// vollständig durchgesetzt (jeder Ausgang außer
    /// `landlock::RulesetStatus::FullyEnforced`, einschließlich eines nicht
    /// öffenbaren Wurzelverzeichnisses). Für dieses Binary immer ein harter
    /// Startfehler — siehe `crate::landlock`-Moduldoku, Abschnitt „Die harte
    /// Entscheidung".
    #[msg(
        "Landlock hat den Zugriff auf das cgroup-Wurzelverzeichnis nicht vollständig durchgesetzt; Start wird verweigert"
    )]
    LandlockUnavailable,

    /// Der seccomp-Filter ließ sich nicht bauen oder installieren
    /// (unbekannte Architektur, Filter zu groß, `seccomp(2)` abgelehnt —
    /// etwa weil die Unit den Aufruf `seccomp` nicht erlaubt). Wie Landlock
    /// ein harter Startfehler (PL-100 Entscheidung O10, siehe
    /// `crate::seccomp`-Moduldoku).
    ///
    /// # Arguments
    /// - `0` (`seccompiler::Error`): die zugrunde liegende Ursache (ohne
    ///   Pfade oder Inhalte).
    #[msg("der seccomp-Filter ließ sich nicht installieren; Start wird verweigert: {0}")]
    SeccompUnavailable(#[source] seccompiler::Error),

    /// Die IPC-Annahmeschleife hat sich endgültig beendet (`accept()`
    /// scheitert dauerhaft, z. B. weil systemd den Socket geschlossen hat).
    /// Der Prozess kann danach keine weitere Anfrage mehr annehmen und
    /// beendet sich deshalb mit einem Fehlschlag statt regulär.
    #[msg(
        "die IPC-Annahmeschleife hat sich endgültig beendet; dieser Prozess kann keine weitere Anfrage mehr annehmen"
    )]
    IpcAcceptLoopTerminated,

    /// Policy, Proof-Schlüssel oder StateDirectory ließen sich nicht sicher
    /// laden (unsichere Eigentums-/Modusbits, Syntaxfehler, beschädigtes
    /// Ledger, ...). Die Meldung nennt keinen Pfad und keinen Inhalt.
    ///
    /// # Arguments
    /// - `0` (`harw_dod_warden::WardenError`): die zugrunde liegende Ursache.
    #[msg("warden setup failed: {0}")]
    #[from]
    Setup(harw_dod_warden::WardenError),

    /// `/etc/passwd` ließ sich nicht lesen (für die Auflösung von
    /// `escalator_user`).
    #[msg("die Benutzerdatenbank ist nicht lesbar: {0}")]
    PasswdUnreadable(#[source] std::io::Error),

    /// `/etc/passwd` ist größer als `crate::peer::MAX_PASSWD_LEN`; sie wird
    /// nie abgeschnitten ausgewertet (ein Schnitt in der Escalator-Zeile
    /// ergäbe eine falsche uid).
    #[msg("die Benutzerdatenbank ist zu groß; Peer-Allowlist nicht auflösbar")]
    PasswdTooLarge,

    /// Der konfigurierte Escalator-Benutzer steht nicht in `/etc/passwd`.
    #[msg("der Escalator-Benutzer ist unbekannt; Peer-Allowlist nicht auflösbar")]
    EscalatorUserUnknown,

    /// Der konfigurierte Escalator-Benutzer steht mehrfach oder mit
    /// unlesbarer uid in `/etc/passwd`.
    #[msg("der Escalator-Benutzer ist nicht eindeutig auflösbar")]
    EscalatorUserAmbiguous,

    /// Die aufgelöste Escalator-uid ist `0` — root ist nie der Escalator.
    #[msg("die Escalator-uid darf nicht root sein")]
    EscalatorUidRoot,

    /// `warden.toml` erlaubt Netzaktionen, aber `CAP_NET_ADMIN` fehlt in
    /// der Ambient-Menge des Prozesses: das `nft`-Kind könnte keine Regel
    /// setzen. Die Vorgabe-Unit gibt die Capability nicht; ein Host mit
    /// Netzisolation braucht das Drop-in (siehe `harw-warden.service`).
    #[msg(
        "warden.toml erlaubt Netzaktionen, aber CAP_NET_ADMIN fehlt in der Ambient-Menge (Isolations-Drop-in der Unit fehlt)"
    )]
    NetAdminMissing,

    /// Der Hold-Timer-Thread ließ sich nicht starten.
    #[msg("der Hold-Timer-Thread ließ sich nicht starten: {0}")]
    HoldTimerSpawn(#[source] std::io::Error),

    /// Das Unterkommando `completions` ist fehlgeschlagen (läuft vor jedem
    /// systemd-, Landlock- oder Socket-Schritt).
    ///
    /// # Arguments
    /// - `0` (`harw_completions::CompletionError`): die zugrunde liegende
    ///   Ursache (Shell nicht erkennbar, fremde Zieldatei, I/O-Fehler).
    #[msg("shell completions failed: {0}")]
    #[from]
    Completions(harw_completions::CompletionError),

    /// Das Unterkommando `keygen` bekam aus der Zufallsquelle des Kernels
    /// keinen vollständigen Schlüssel oder konnte seine (inhaltsfreie)
    /// Meldung nicht schreiben. Schreib- und Prüffehler der Schlüsseldatei
    /// selbst sind [`Self::Setup`].
    ///
    /// # Arguments
    /// - `0` (`std::io::Error`): die Ursache (nie Schlüsselmaterial).
    #[msg("proof key generation failed: {0}")]
    KeyGeneration(#[source] std::io::Error),
}

/// Formatiert `WardenBinError` über seine [`std::fmt::Display`]-Meldung.
///
/// # Description
/// `#[derive(harw_macros::HarwError)]` erzeugt kein `Debug` — diese Impl
/// schließt die Lücke, indem sie an `Display` delegiert, statt eine zweite,
/// potenziell auseinanderlaufende Formatierung zu pflegen (Contract-Master
/// §H.1-Konvention, dieselbe wie bei `harw_dod_warden::error::WardenError`).
impl std::fmt::Debug for WardenBinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::WardenBinError;

    #[test]
    fn test_listen_pid_foreign_display_is_fixed_and_content_free() {
        let err = WardenBinError::ListenPidForeign;
        assert!(err.to_string().contains("LISTEN_PID"));
        assert!(err.source().is_none());
    }

    #[test]
    fn test_unexpected_listen_fd_count_interpolates_actual() {
        let err = WardenBinError::UnexpectedListenFdCount { actual: 3 };
        assert!(err.to_string().contains('3'));
    }

    #[test]
    fn test_listen_fds_acquisition_failed_source_links_to_inner_error() {
        let err = WardenBinError::from(sd_listen_fds::Error::MalformedEnv);
        assert!(err.source().is_some());
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let err = WardenBinError::LandlockUnavailable;
        assert_eq!(format!("{err:?}"), err.to_string());
    }

    #[test]
    fn test_setup_and_peer_errors_are_content_free() {
        let setup = WardenBinError::from(harw_dod_warden::WardenError::KeyStore {
            reason: "no proof keys found",
        });
        assert_eq!(
            setup.to_string(),
            "warden setup failed: proof key store ist unbrauchbar: no proof keys found"
        );
        assert!(setup.source().is_some());
        assert!(WardenBinError::EscalatorUidRoot.source().is_none());
        assert!(WardenBinError::PasswdTooLarge.source().is_none());
        assert!(!WardenBinError::PasswdTooLarge.to_string().contains('/'));
        let unreadable = WardenBinError::PasswdUnreadable(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        ));
        assert!(unreadable.source().is_some());
    }

    #[test]
    fn test_seccomp_unavailable_links_the_cause_and_names_no_path() {
        let err = WardenBinError::SeccompUnavailable(seccompiler::Error::EmptyFilter);
        assert!(err.source().is_some());
        assert!(err.to_string().starts_with("der seccomp-Filter"));
        assert!(!err.to_string().contains('/'));
    }

    #[test]
    fn test_key_generation_links_the_cause() {
        let err =
            WardenBinError::KeyGeneration(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        assert!(err.source().is_some());
        assert!(err.to_string().starts_with("proof key generation failed"));
    }

    #[test]
    fn test_ipc_accept_loop_terminated_has_a_fixed_message() {
        assert_eq!(
            WardenBinError::IpcAcceptLoopTerminated.to_string(),
            "die IPC-Annahmeschleife hat sich endgültig beendet; dieser Prozess kann keine weitere Anfrage mehr annehmen"
        );
    }
}
