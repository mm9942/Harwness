//! Plattform-Politik für Host-Ausführung (Android-Anbindung).
//!
//! # Verantwortungsbereich
//! Auf Android gibt es weder `bwrap` noch Nutzer-Namensräume — die Bubblewrap-
//! Sandbox ([`super::run_command`]) kann dort nicht laufen. [`ExecPlatform`]
//! trägt die **einzige** `cfg!(target_os = "android")`-Abfrage dieses Moduls;
//! jede andere Stelle im Crate entscheidet ausschließlich über den daraus
//! abgeleiteten Wert (Konstruktion oder [`ShellToolProvider::with_exec_platform`]),
//! nie erneut über `cfg!`. Das hält den Unterschied zwischen Linux (und jedem
//! anderen Nicht-Android-Ziel) und Android an einer einzigen Stelle sichtbar
//! und macht [`host_policy`] auf Linux vollständig testbar
//! ([`ExecPlatform::NoSandbox`] lässt sich dort explizit erzwingen).
//!
//! [`host_policy`] ist eine reine Funktion ohne Seiteneffekt: sie liest weder
//! Dateisystem noch Umgebung, sondern entscheidet ausschließlich aus den drei
//! übergebenen Werten. Die eigentliche Durchsetzung (Freigabekanal, Registry,
//! Prompt) bleibt in [`super::ShellExecutor::determine_effective_host`].

use harw_extension_api::ApprovalMode;

/// Ob dieser Build auf einer Plattform ohne Bubblewrap-Sandbox läuft.
///
/// # Beschreibung
/// [`Self::current`] ist die einzige Stelle, die `cfg!(target_os =
/// "android")` auswertet. Jeder andere Aufrufer (Konstruktion, Tests) trägt
/// diesen Wert nur weiter — insbesondere legt [`Self::current`] nie fest, ob
/// tatsächlich ohne Rückfrage ausgeführt wird: das entscheidet [`host_policy`]
/// zusammen mit dem [`ApprovalMode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecPlatform {
    /// Bubblewrap/Nutzer-Namensräume sind verfügbar (Linux und jedes andere
    /// Nicht-Android-Ziel). Verhalten bleibt byte-identisch zum bisherigen
    /// Stand: Host-Ausführung läuft ausschließlich über den bestehenden
    /// Freigabeweg ([`super::ShellExecutor::authorize_host_command`] bzw. die
    /// Sitzungs-/Einmalfreigabe der [`harw_sandbox::HostPermitSessionRegistry`]).
    Sandboxed,
    /// Keine Bubblewrap-Sandbox verfügbar (Android/Termux). Jede
    /// Host-Ausführung braucht eine ausdrückliche Freigabe (Sitzungsphase,
    /// Einmalfreigabe oder Rückfrage) — außer der Freigabemodus steht auf
    /// [`ApprovalMode::FullAccess`].
    NoSandbox,
}

impl ExecPlatform {
    /// Die Plattform dieses Builds. Einzige Stelle mit
    /// `cfg!(target_os = "android")` im gesamten Crate.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "android") {
            Self::NoSandbox
        } else {
            Self::Sandboxed
        }
    }
}

/// Interne Politik, wie ein Aufruf zu seiner Host-Entscheidung kommt.
///
/// # Beschreibung
/// Reines Ergebnis von [`host_policy`] — trifft selbst keine Entscheidung,
/// sondern benennt nur, welcher der bestehenden Wege in
/// [`super::ShellExecutor::determine_effective_host`] gilt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostPolicy {
    /// Unverändertes Verhalten: Sandbox, außer eine Sitzungs- oder
    /// Einmalfreigabe der Registry deckt den Aufruf bereits.
    SandboxUnlessLease,
    /// Es gibt keine Sandbox; eine ausdrückliche Freigabe ist nötig
    /// (Sitzungsphase, Einmalfreigabe der Registry, sonst Rückfrage — ohne
    /// Fragekanal fail-closed).
    RequireApproval,
    /// Es gibt keine Sandbox, aber der Freigabemodus erlaubt Ausführung ohne
    /// jede Rückfrage.
    HostWithoutPrompt,
}

/// Reine Entscheidungsfunktion: welche [`HostPolicy`] gilt für diese
/// Kombination aus Plattform, Sandbox-Profil und Freigabemodus.
///
/// # Beschreibung
/// - [`ExecPlatform::Sandboxed`]: `mode` wird nicht ausgewertet (Linux-
///   Verhalten bleibt unverändert, unabhängig vom Freigabemodus). Ist
///   `profile_is_host`, gilt [`HostPolicy::RequireApproval`] — das ist genau
///   der heutige `authorize_host_command`-Pfad. Sonst
///   [`HostPolicy::SandboxUnlessLease`] — der heutige Bubblewrap-Pfad mit
///   Sitzungs-/Einmalfreigabe-Kurzschluss.
/// - [`ExecPlatform::NoSandbox`]: `mode == Some(ApprovalMode::FullAccess)`
///   ⇒ [`HostPolicy::HostWithoutPrompt`] (kein Sandbox-Pfad existiert dort,
///   also ist eine Rückfrage die einzige Alternative). Jeder andere Modus
///   (oder kein Modus angehängt) ⇒ [`HostPolicy::RequireApproval`].
///
/// `mode = None` heißt: keine [`harw_extension_api::approval_mode::ApprovalModeCell`]
/// angehängt (z. B. `ShellToolProvider::default()` ohne
/// [`super::ShellToolProvider::with_approval_mode`]) — verhält sich wie jeder
/// nicht-[`ApprovalMode::FullAccess`]-Modus.
pub(super) fn host_policy(
    platform: ExecPlatform,
    profile_is_host: bool,
    mode: Option<ApprovalMode>,
) -> HostPolicy {
    match platform {
        ExecPlatform::Sandboxed => {
            if profile_is_host {
                HostPolicy::RequireApproval
            } else {
                HostPolicy::SandboxUnlessLease
            }
        }
        ExecPlatform::NoSandbox => match mode {
            Some(ApprovalMode::FullAccess) => HostPolicy::HostWithoutPrompt,
            _ => HostPolicy::RequireApproval,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vollständige Wahrheitstabelle: `mode` wird auf [`ExecPlatform::Sandboxed`]
    /// nie ausgewertet (Linux-Verhalten bleibt unverändert), unabhängig vom
    /// Sandbox-Profil.
    #[test]
    fn test_host_policy_truth_table() {
        let cases = [
            // (platform, profile_is_host, mode) -> expected
            (
                ExecPlatform::Sandboxed,
                true,
                None,
                HostPolicy::RequireApproval,
            ),
            (
                ExecPlatform::Sandboxed,
                true,
                Some(ApprovalMode::FullAccess),
                HostPolicy::RequireApproval,
            ),
            (
                ExecPlatform::Sandboxed,
                false,
                None,
                HostPolicy::SandboxUnlessLease,
            ),
            (
                ExecPlatform::Sandboxed,
                false,
                Some(ApprovalMode::FullAccess),
                HostPolicy::SandboxUnlessLease,
            ),
            (
                ExecPlatform::NoSandbox,
                true,
                Some(ApprovalMode::FullAccess),
                HostPolicy::HostWithoutPrompt,
            ),
            (
                ExecPlatform::NoSandbox,
                false,
                Some(ApprovalMode::FullAccess),
                HostPolicy::HostWithoutPrompt,
            ),
            (
                ExecPlatform::NoSandbox,
                false,
                Some(ApprovalMode::Delegated),
                HostPolicy::RequireApproval,
            ),
            (
                ExecPlatform::NoSandbox,
                false,
                Some(ApprovalMode::AlwaysAsk),
                HostPolicy::RequireApproval,
            ),
            (
                ExecPlatform::NoSandbox,
                true,
                None,
                HostPolicy::RequireApproval,
            ),
            (
                ExecPlatform::NoSandbox,
                false,
                None,
                HostPolicy::RequireApproval,
            ),
        ];

        for (platform, profile_is_host, mode, expected) in cases {
            assert_eq!(
                host_policy(platform, profile_is_host, mode),
                expected,
                "platform={platform:?} profile_is_host={profile_is_host} mode={mode:?}"
            );
        }
    }

    #[test]
    fn test_current_matches_cfg_target_os() {
        let expected = if cfg!(target_os = "android") {
            ExecPlatform::NoSandbox
        } else {
            ExecPlatform::Sandboxed
        };
        assert_eq!(ExecPlatform::current(), expected);
    }
}
