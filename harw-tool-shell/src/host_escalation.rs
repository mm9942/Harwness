//! Host-Mode-Anfrage aus dem Orchestrator-Baum (Runde 5, Teil N).
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt die **öffentlichen** Bausteine, mit denen ein
//! `shell.exec`-Aufruf eines beliebigen Shell-fähigen Agenten (Wurzel oder
//! Kind, Strict-/Cargo-/Tmux-Profil) ausdrücklich Host-Mode anfragen kann:
//!
//! - [`HostEscalation`] — die Verdrahtung, die nur die interaktive TUI
//!   anhängt (`EntryKind::Tui`). Fehlt sie, bleibt jede Anfrage fail-closed
//!   mit [`HOST_MODE_REQUIRES_TUI_MSG`].
//! - [`HostRequesterBook`] / [`HostRequester`] — wer fragt: Rolle, Kind-ID und
//!   Pfad im Agentenbaum (z. B. `uia › root-orchestrator › executor`). Die
//!   Runtime trägt Wurzel und Kinder ein (`harw-runtime`,
//!   `host_escalation_wiring`), der Executor liest nur.
//! - [`SandboxDenial`] / [`classify_sandbox_denial`] — Heuristik, die einen
//!   gescheiterten Sandbox-Lauf als Sandbox-Grenze erkennt (Netz, Pfad
//!   außerhalb, Namespace/Syscall, fehlendes Programm) und dem Modell den Weg
//!   zur Anfrage zeigt ([`denial_hint`]).
//! - [`audit_escalation`] — strukturierter Audit-Eintrag je Anfrage und
//!   Entscheidung, im selben `tracing`-Stil wie das bestehende
//!   Host-Permit-Audit.
//!
//! Die eigentliche Ablaufsteuerung (Anfrage stellen, Permit ausstellen, auf
//! dem Host ausführen) lebt in `crate::exec::escalation`, weil sie an die
//! privaten Teile von [`crate::ShellExecutor`] heranmuss.
//!
//! # Sicherheitsregeln
//! - Nur wer ohnehin `shell.exec` hat (also `ExecuteProcess`), kann fragen;
//!   Read-only-Rollen bekommen weiterhin nie ein Shell-Werkzeug.
//! - Ablehnung, Zeitablauf, geschlossener Kanal: fail-closed, wie bei jeder
//!   [`crate::HostPermitPrompt`].
//! - Rechte-Werkzeuge (`sudo`, `doas`, `pkexec`, …) bleiben auch im Host-Mode
//!   verboten; die Prüfung läuft vor jeder Anfrage.
//! - Das Modell kann weder Rolle noch Pfad noch Vorauswahl setzen — nur den
//!   Grund (`request_host.reason`), der angezeigt, aber nie ausgewertet wird.
//!
//! # Concurrency
//! [`HostRequesterBook`] ist `Send + Sync` (ein [`Mutex`]); ein vergifteter
//! Mutex liefert fail-safe den Rückfall-Anfragenden statt zu paniken.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

/// Trenner zwischen den Knoten eines Baum-Pfads in der Anzeige.
pub const REQUESTER_PATH_SEPARATOR: &str = " › ";

/// Beschriftung der Wurzel, solange die Runtime keine eigene eingetragen hat.
pub const ROOT_REQUESTER_FALLBACK: &str = "Hauptsitzung";

/// Meldung, wenn eine Host-Mode-Anfrage ohne Fragekanal (kein TUI-Einstieg)
/// gestellt wird — fail-closed, aber verständlich.
pub const HOST_MODE_REQUIRES_TUI_MSG: &str = "shell.exec: Host-Mode nötig – nur in der TUI mit \
     Freigabe möglich. Dieser Lauf hat keinen Freigabekanal (kein interaktiver TUI-Einstieg); \
     der Befehl bleibt in der Sandbox (fail-closed).";

/// Meldung für jeden Ausgang einer geöffneten Host-Mode-Anfrage, der keine
/// ausdrückliche Zustimmung ist.
pub const HOST_ESCALATION_DENIED_MSG: &str = "shell.exec: Host-Mode wurde nicht freigegeben \
     (abgelehnt, Zeit abgelaufen oder Antwort verworfen); der Befehl lief nicht.";

/// Worker-Definition, unter der eine Host-Mode-Anfrage im Permit-Ledger
/// geführt wird (Präfix; die anfragende Rolle wird angehängt).
pub const HOST_ESCALATION_WORKER_PREFIX: &str = "host-escalation@1";

/// Größte zulässige Länge des vom Modell gelieferten Grundes (Zeichen).
pub const HOST_ESCALATION_REASON_MAX_CHARS: usize = 500;

/// Die Verdrahtung für Host-Mode-Anfragen aus dem Agentenbaum.
///
/// # Description
/// Nur die interaktive TUI hängt sie an (siehe
/// `harw_registry_defaults::profile::HostPermitWiring::with_host_escalation`).
/// Sie trägt das gemeinsame [`HostRequesterBook`], damit jede Anfrage zeigen
/// kann, **wer** fragt. Ohne diese Verdrahtung beantwortet `shell.exec` jede
/// `request_host`-Anfrage mit [`HOST_MODE_REQUIRES_TUI_MSG`].
///
/// # Concurrency
/// `Clone` teilt dasselbe Buch ([`Arc`]).
#[derive(Debug, Clone, Default)]
pub struct HostEscalation {
    requesters: Arc<HostRequesterBook>,
}

impl HostEscalation {
    /// Baut eine Verdrahtung mit frischem, leerem Buch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Das gemeinsame Buch der Anfragenden.
    #[must_use]
    pub fn requesters(&self) -> &Arc<HostRequesterBook> {
        &self.requesters
    }
}

/// Wer eine Host-Mode-Anfrage stellt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRequester {
    /// Rollenname (z. B. `executor`) bzw. die Wurzel-Beschriftung.
    pub role: String,
    /// Sitzungs-ID des anfragenden Agenten (Kind-ID).
    pub session: String,
    /// Pfad im Agentenbaum, z. B. `uia › root-orchestrator › executor`.
    pub path: String,
}

impl fmt::Display for HostRequester {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (Rolle {}, Sitzung {})",
            self.path, self.role, self.session
        )
    }
}

#[derive(Debug, Clone)]
struct BookEntry {
    role: String,
    path: String,
}

#[derive(Debug, Default)]
struct BookState {
    /// Sitzung → Rolle und Pfad (Wurzel und laufende Kinder).
    entries: HashMap<String, BookEntry>,
    /// Zuletzt montiertes Kind `(Eltern-Sitzung, Rolle)`, bis
    /// [`HostRequesterBook::bind_child`] seine Sitzungs-ID nachreicht. Der
    /// Controller baut Registry, Sitzung und Beobachter eines Kindes unter
    /// demselben `SessionManager`-Lock nacheinander — zwei Admissions
    /// verschränken sich dabei nie.
    pending: Option<(String, String)>,
}

/// Verzeichnis der Anfragenden: welche Sitzung welche Rolle an welcher Stelle
/// im Agentenbaum ist.
///
/// # Description
/// Die Runtime trägt die Wurzel ([`Self::register_root`]) und jedes Kind
/// ([`Self::note_spawn`] beim Registry-Bau, [`Self::bind_child`] sobald die
/// Kind-Sitzung existiert) ein und entfernt Kinder bei ihrer Freigabe
/// ([`Self::release`]). Der Executor liest nur ([`Self::requester_for`]).
/// Unbekannte Sitzungen fallen fail-safe auf einen Eintrag mit Rolle
/// `unbekannt` zurück — die Anfrage geht trotzdem raus, zeigt aber ehrlich,
/// dass die Herkunft nicht bekannt ist.
#[derive(Debug, Default)]
pub struct HostRequesterBook {
    state: Mutex<BookState>,
}

impl HostRequesterBook {
    /// Trägt die Wurzelsitzung mit ihrer Anzeige-Beschriftung ein (z. B.
    /// `uia` oder `Hauptsitzung`).
    pub fn register_root(&self, session: impl Into<String>, label: impl Into<String>) {
        let label = label.into();
        if let Ok(mut state) = self.state.lock() {
            state.entries.insert(
                session.into(),
                BookEntry {
                    role: label.clone(),
                    path: label,
                },
            );
        }
    }

    /// Merkt sich, dass für `parent_session` gerade ein Kind der Rolle `role`
    /// montiert wird (Registry-Bau, vor der Sitzungserzeugung).
    pub fn note_spawn(&self, parent_session: &str, role: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.pending = Some((parent_session.to_owned(), role.to_owned()));
        }
    }

    /// Bindet die soeben erzeugte Kind-Sitzung an den vorgemerkten Eintrag.
    ///
    /// # Description
    /// Passt die Rolle des vorgemerkten Eintrags nicht (etwa weil eine
    /// Montage zwischendurch scheiterte), wird das Kind trotzdem eingetragen —
    /// mit einem Pfad, der die unbekannte Herkunft offen zeigt.
    pub fn bind_child(&self, role: &str, child_session: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let parent_path = match state.pending.take() {
            Some((parent, pending_role)) if pending_role == role => state
                .entries
                .get(&parent)
                .map(|entry| entry.path.clone())
                .unwrap_or_else(|| ROOT_REQUESTER_FALLBACK.to_owned()),
            _ => "?".to_owned(),
        };
        let path = format!("{parent_path}{REQUESTER_PATH_SEPARATOR}{role}");
        state.entries.insert(
            child_session.to_owned(),
            BookEntry {
                role: role.to_owned(),
                path,
            },
        );
    }

    /// Entfernt ein freigegebenes Kind (Abschluss, Abbruch, Fehler).
    pub fn release(&self, child_session: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.entries.remove(child_session);
        }
    }

    /// Der Anfragende für `session`.
    ///
    /// # Returns
    /// Den eingetragenen Anfragenden, sonst einen Rückfall mit Rolle
    /// `unbekannt` (auch bei vergiftetem Mutex — fail-safe, nie Panik).
    #[must_use]
    pub fn requester_for(&self, session: &str) -> HostRequester {
        let entry = self
            .state
            .lock()
            .ok()
            .and_then(|state| state.entries.get(session).cloned());
        match entry {
            Some(entry) => HostRequester {
                role: entry.role,
                session: session.to_owned(),
                path: entry.path,
            },
            None => HostRequester {
                role: "unbekannt".to_owned(),
                session: session.to_owned(),
                path: format!("{ROOT_REQUESTER_FALLBACK}{REQUESTER_PATH_SEPARATOR}?"),
            },
        }
    }
}

/// Argumente des optionalen `shell.exec`-Felds `request_host`.
///
/// # Description
/// `{"reason": "…"}` — der Grund wird angezeigt und auditiert, aber nie
/// ausgewertet. Unbekannte Felder werden abgelehnt.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequestHostArgs {
    /// Warum Host-Mode nötig ist (Pflicht, nicht leer).
    pub reason: String,
}

impl RequestHostArgs {
    /// Prüft den Grund und kürzt ihn auf [`HOST_ESCALATION_REASON_MAX_CHARS`].
    ///
    /// # Errors
    /// Eine lesbare Meldung, wenn der Grund leer ist.
    pub fn normalized_reason(&self) -> Result<String, String> {
        let trimmed = self.reason.trim();
        if trimmed.is_empty() {
            return Err("request_host.reason must not be blank".to_owned());
        }
        Ok(trimmed
            .chars()
            .filter(|ch| !ch.is_control() || *ch == ' ')
            .take(HOST_ESCALATION_REASON_MAX_CHARS)
            .collect())
    }
}

/// Erkannte Sandbox-Grenze eines gescheiterten Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDenial {
    /// Netz verweigert (DNS, Verbindung, `--offline`).
    Network,
    /// Pfad außerhalb der Sandbox bzw. schreibgeschützt eingebunden.
    OutsideWorkspace,
    /// Namespace-/Syscall-Grenze (`Operation not permitted`, `bwrap`, seccomp).
    Namespace,
    /// Programm in der Sandbox nicht vorhanden bzw. vom Profil nicht erlaubt.
    ProgramMissing,
    /// Die Sandbox selbst ließ sich nicht aufbauen.
    SandboxSetup,
}

impl SandboxDenial {
    /// Stabiler Maschinenname (JSON-Feld `sandbox_denial`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::OutsideWorkspace => "outside_workspace",
            Self::Namespace => "namespace",
            Self::ProgramMissing => "program_missing",
            Self::SandboxSetup => "sandbox_setup",
        }
    }

    /// Deutschsprachige Kurzbeschreibung.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Network => "Netzzugriff in der Sandbox verweigert",
            Self::OutsideWorkspace => "Pfad außerhalb der Sandbox oder schreibgeschützt",
            Self::Namespace => "Namespace-/Syscall-Grenze der Sandbox",
            Self::ProgramMissing => "Programm in der Sandbox nicht verfügbar",
            Self::SandboxSetup => "Sandbox ließ sich nicht aufbauen",
        }
    }
}

const NETWORK_MARKERS: &[&str] = &[
    "could not resolve host",
    "temporary failure in name resolution",
    "name or service not known",
    "network is unreachable",
    "failed to lookup address information",
    "failed to connect to",
    "--offline",
    "spurious network error",
    "failed to download",
    "dns error",
    "no route to host",
];

const OUTSIDE_MARKERS: &[&str] = &[
    "read-only file system",
    "permission denied",
    "cannot create directory",
];

const NAMESPACE_MARKERS: &[&str] = &[
    "operation not permitted",
    "bad system call",
    "bwrap:",
    "seccomp",
    "unshare",
    "setns",
    "creating new namespace failed",
];

const MISSING_MARKERS: &[&str] = &[
    "command not found",
    ": not found",
    "no such file or directory",
];

/// Erkennt anhand von Exit-Code und Ausgabe, ob ein Sandbox-Lauf an der
/// Sandbox-Grenze scheiterte.
///
/// # Description
/// Reine Heuristik — sie entscheidet nie über Ausführung, sondern nur über
/// den Hinweis an das Modell. Exit-Code `0` ergibt immer `None`. Reihenfolge:
/// Netz, Namespace, Pfad, fehlendes Programm (Exit 127 zählt immer als
/// fehlendes Programm).
#[must_use]
pub fn classify_sandbox_denial(
    exit_code: i64,
    stdout: &str,
    stderr: &str,
) -> Option<SandboxDenial> {
    if exit_code == 0 {
        return None;
    }
    let haystack = format!("{stderr}\n{stdout}").to_lowercase();
    let contains_any = |markers: &[&str]| markers.iter().any(|marker| haystack.contains(marker));
    if contains_any(NETWORK_MARKERS) && !haystack.contains("offline mode is enabled on purpose") {
        return Some(SandboxDenial::Network);
    }
    if contains_any(NAMESPACE_MARKERS) {
        return Some(SandboxDenial::Namespace);
    }
    if contains_any(OUTSIDE_MARKERS) {
        return Some(SandboxDenial::OutsideWorkspace);
    }
    if exit_code == 127 || contains_any(MISSING_MARKERS) {
        return Some(SandboxDenial::ProgramMissing);
    }
    None
}

/// Hinweistext für das Modell nach einer erkannten Sandbox-Grenze.
///
/// # Arguments
/// - `denial` ([`SandboxDenial`]): die erkannte Grenze.
/// - `can_request` (`bool`): ob dieser Lauf einen Freigabekanal hat
///   ([`HostEscalation`] angehängt).
#[must_use]
pub fn denial_hint(denial: SandboxDenial, can_request: bool) -> String {
    if can_request {
        format!(
            "Möglicherweise Sandbox-Grenze ({}). Falls der Befehl wirklich auf dem Host laufen \
             muss: shell.exec erneut mit \"request_host\": {{\"reason\": \"<kurzer Grund>\"}} \
             aufrufen — die Nutzerin wird dann gefragt (einmalig oder Host-Arbeitsphase). \
             sudo/doas/pkexec bleiben verboten.",
            denial.label()
        )
    } else {
        format!(
            "Möglicherweise Sandbox-Grenze ({}). Host-Mode nötig – nur in der TUI mit Freigabe \
             möglich; dieser Lauf hat keinen Freigabekanal.",
            denial.label()
        )
    }
}

/// Ergebnis einer Host-Mode-Anfrage für das Audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationOutcome {
    /// Frage gestellt, Antwort steht aus.
    Requested,
    /// Einmalig freigegeben.
    ApprovedOnce,
    /// Host-Arbeitsphase freigegeben.
    ApprovedSession,
    /// Bereits durch eine laufende Sitzungsphase gedeckt, keine Frage.
    CoveredByLease,
    /// Abgelehnt, Zeit abgelaufen oder Antwort verworfen.
    Denied,
    /// Kein Freigabekanal (kein TUI-Einstieg) — fail-closed.
    NoChannel,
    /// Freigegeben, aber der Permit ließ sich nicht ausstellen.
    PermitFailed,
}

impl EscalationOutcome {
    /// Stabiler Maschinenname für das Audit.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::ApprovedOnce => "approved_once",
            Self::ApprovedSession => "approved_session",
            Self::CoveredByLease => "covered_by_lease",
            Self::Denied => "denied",
            Self::NoChannel => "no_channel",
            Self::PermitFailed => "permit_failed",
        }
    }
}

/// Schreibt einen Audit-Eintrag für eine Host-Mode-Anfrage bzw. deren
/// Entscheidung.
///
/// # Description
/// Gleiches Muster wie das bestehende Host-Permit-Audit
/// (`shell.host_permit.*`-Ereignisse): strukturierte `tracing`-Felder,
/// Ziel `harw::audit`, Ereignisname `shell.host_escalation`. Enthält Rolle,
/// Sitzung, Baum-Pfad, exakten Befehl, cwd, Grund und Ergebnis.
pub fn audit_escalation(
    requester: &HostRequester,
    command: &str,
    cwd: &std::path::Path,
    reason: &str,
    outcome: EscalationOutcome,
) {
    tracing::info!(
        target: "harw::audit",
        role = %requester.role,
        session = %requester.session,
        path = %requester.path,
        command = %command,
        cwd = %cwd.display(),
        reason = %reason,
        outcome = outcome.as_str(),
        "shell.host_escalation"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_book_builds_tree_path_from_root_over_children() {
        let book = HostRequesterBook::default();
        book.register_root("root", "uia");
        book.note_spawn("root", "root-orchestrator");
        book.bind_child("root-orchestrator", "child-1");
        book.note_spawn("child-1", "executor");
        book.bind_child("executor", "child-2");

        let requester = book.requester_for("child-2");
        assert_eq!(requester.role, "executor");
        assert_eq!(requester.session, "child-2");
        assert_eq!(requester.path, "uia › root-orchestrator › executor");
        assert_eq!(book.requester_for("root").path, "uia");
    }

    #[test]
    fn test_book_falls_back_for_unknown_and_released_sessions() {
        let book = HostRequesterBook::default();
        book.note_spawn("root", "executor");
        book.bind_child("executor", "child-1");
        assert_eq!(
            book.requester_for("child-1").path,
            format!("{ROOT_REQUESTER_FALLBACK} › executor")
        );
        book.release("child-1");
        assert_eq!(book.requester_for("child-1").role, "unbekannt");
    }

    #[test]
    fn test_book_marks_mismatched_pending_role_as_unknown_origin() {
        let book = HostRequesterBook::default();
        book.note_spawn("root", "planner");
        book.bind_child("executor", "child-1");
        assert_eq!(book.requester_for("child-1").path, "? › executor");
    }

    #[test]
    fn test_reason_must_not_be_blank_and_is_capped() {
        let blank = RequestHostArgs {
            reason: "   ".to_owned(),
        };
        assert!(blank.normalized_reason().is_err());
        let long = RequestHostArgs {
            reason: "x".repeat(HOST_ESCALATION_REASON_MAX_CHARS + 50),
        };
        assert_eq!(
            long.normalized_reason()
                .map(|reason| reason.chars().count()),
            Ok(HOST_ESCALATION_REASON_MAX_CHARS)
        );
    }

    #[test]
    fn test_classify_sandbox_denial_recognizes_the_four_limits() {
        assert_eq!(
            classify_sandbox_denial(0, "", "Could not resolve host"),
            None
        );
        assert_eq!(
            classify_sandbox_denial(
                101,
                "",
                "error: failed to download from `https://index.crates.io`"
            ),
            Some(SandboxDenial::Network)
        );
        assert_eq!(
            classify_sandbox_denial(1, "", "touch: cannot touch '/srv/x': Read-only file system"),
            Some(SandboxDenial::OutsideWorkspace)
        );
        assert_eq!(
            classify_sandbox_denial(1, "", "unshare: Operation not permitted"),
            Some(SandboxDenial::Namespace)
        );
        assert_eq!(
            classify_sandbox_denial(127, "", "sh: 1: rg: not found"),
            Some(SandboxDenial::ProgramMissing)
        );
        assert_eq!(
            classify_sandbox_denial(1, "", "test failed: assertion"),
            None
        );
    }

    #[test]
    fn test_denial_hint_names_the_request_field_or_the_tui_requirement() {
        assert!(denial_hint(SandboxDenial::Network, true).contains("request_host"));
        assert!(denial_hint(SandboxDenial::Network, false).contains("nur in der TUI"));
    }
}
