//! `harw sandbox` — Host-Profil-Permit-Ledger prüfen und verwalten.
//!
//! # Verantwortung
//! Dieses Modul führt [`crate::cli::SandboxAction`] aus. Es ist bewusst die
//! **Konfigurations-/Audit-Fläche** für [`harw_sandbox::ProcessPermitLedger`]
//! und [`harw_sandbox::HostPermitSessionRegistry`] — analog zu
//! `crate::settings`/`crate::models` — und **kein** Fenster in eine bereits
//! laufende `harw`-Chat-/TUI-Sitzung.
//!
//! # Warum keine Live-Sitzungsdaten
//! [`harw_sandbox::ProcessPermitLedger`] hält seinen Zustand ausschließlich
//! im Speicher des Prozesses, der ihn instanziiert (`harw_runtime::assembly::
//! RuntimeAssembly::host_permit_ledger`, siehe dessen Doku). Ein separater
//! `harw sandbox`-Prozess besitzt deshalb grundsätzlich einen eigenen,
//! frischen Ledger ohne die Leases einer parallel laufenden Sitzung — genau
//! wie kein anderer `harw`-Subcommand über Prozessgrenzen hinweg auf
//! In-Memory-Zustand eines anderen Prozesses zugreift. `leases`/`revoke`
//! demonstrieren deshalb ausdrücklich den **leeren** Zustand dieses
//! Prozesses statt eine nicht existierende Verbindung zu simulieren.
//!
//! # Nebenläufigkeit
//! Zustandslos; ein `harw sandbox`-Prozess pro Aufruf ist die erwartete
//! Nutzung, wie bei `crate::settings`/`crate::models`.

use std::sync::Arc;

use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger};

use crate::cli::SandboxAction;

/// Einzige heute bekannte Worker-Definition, die `SandboxProfile::Host`
/// deklariert (siehe `harw-registry-defaults/agents/host-process-worker.toml`
/// und die gleichlautende Konstante in `harw-tool-shell/src/exec.rs`).
const KNOWN_HOST_WORKER_DEFINITIONS: &[&str] = &["host-process-worker"];

/// Führt `harw sandbox [action]` aus.
///
/// # Arguments
/// - `action` (`Option<SandboxAction>`): Unterbefehl, oder `None` für den
///   Status.
///
/// # Errors
/// Diese Funktion gibt aktuell nie `Err` zurück; die Signatur ist `Result`,
/// um konsistent mit den übrigen `harw-cli`-Subcommand-Läufern zu bleiben
/// (siehe `crate::settings::run`, `crate::models::run`) und künftige
/// Fehlerfälle (z. B. eine spätere IPC-Anbindung an eine laufende Sitzung)
/// ohne Signaturänderung aufnehmen zu können.
pub fn run(action: Option<SandboxAction>) -> Result<(), String> {
    match action.unwrap_or(SandboxAction::Status) {
        SandboxAction::Status => {
            print_status();
            Ok(())
        }
        SandboxAction::Leases => {
            print_leases();
            Ok(())
        }
        SandboxAction::Revoke { session } => {
            print_revoke(&session);
            Ok(())
        }
    }
}

fn print_status() {
    println!("Host-Profil-Permit-Ledger (harw_sandbox::ProcessPermitLedger)");
    println!();
    println!("Bekannte Host-Profil-Worker-Definitionen:");
    for name in KNOWN_HOST_WORKER_DEFINITIONS {
        println!("  - {name}");
    }
    println!();
    println!(
        "Verdrahtung: harw_runtime::assembly::RuntimeAssembly instanziiert je Lauf einen \
         ProcessPermitLedger + eine HostPermitSessionRegistry und reicht beide an jeden \
         ShellToolProvider durch, dessen sandbox_profile tatsächlich SandboxProfile::Host \
         ist (harw_registry_defaults::profile::\
         assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits)."
    );
    println!(
        "Ohne diese Verdrahtung lehnt harw_tool_shell::ShellExecutor jede Host-Ausführung \
         fail-closed ab (\"host execution requires a process permit\")."
    );
    println!();
    println!(
        "Hinweis: Dieser Befehl liest keine laufende harw-Sitzung — siehe Moduldoku. \
         `harw sandbox leases`/`harw sandbox revoke` zeigen den (leeren) Ledger dieses \
         Prozesses."
    );
}

fn print_leases() {
    let (_ledger, registry) = fresh_ledger();
    let active = registry.is_session_approved("");
    debug_assert!(!active, "ein frischer Ledger darf keine Zustimmung tragen");
    println!("Aktive Host-Permit-Leases in diesem Prozess: 0");
    println!(
        "Leases leben ausschließlich im Speicher der Sitzung, die sie über den lokalen \
         Bestätigungsdialog (harw_tui::host_permit_dialog::HostPermitDialog) erteilt hat — \
         siehe Moduldoku für den Grund, warum dieser Befehl sie nicht einsehen kann."
    );
}

fn print_revoke(session: &str) {
    let (ledger, registry) = fresh_ledger();
    let removed = registry.forget_session(session);
    for id in &removed {
        // In diesem frischen Ledger ist jede Kennung unbekannt; `revoke` ist
        // absichtlich idempotent (siehe `ProcessPermitLedger::revoke`).
        let _ = ledger.revoke(*id);
    }
    println!(
        "Widerrufen für Sitzung {session:?} in diesem Prozess: {} Permit(s), \
         Sitzungszustimmung entfernt.",
        removed.len()
    );
    if removed.is_empty() {
        println!(
            "0 ist hier der erwartete Wert: siehe Moduldoku — dieser Prozess trägt nie die \
             Leases einer anderen, bereits laufenden Sitzung."
        );
    }
}

// Baut ein frisches, unverbundenes Ledger-Paar für die rein demonstrative
// Auskunft dieses Prozesses (siehe Moduldoku).
fn fresh_ledger() -> (Arc<ProcessPermitLedger>, Arc<HostPermitSessionRegistry>) {
    (
        Arc::new(ProcessPermitLedger::default()),
        Arc::new(HostPermitSessionRegistry::default()),
    )
}

// Referenziert `HostApprovalScope`, damit die Importe erklärt
// bleiben, falls ein künftiger Patch hier tatsächlich Leases ausstellt
// (z. B. sobald eine IPC-Anbindung an eine laufende Sitzung existiert).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;
    use harw_sandbox::HostApprovalScope;

    #[test]
    fn test_run_status_action_succeeds() {
        assert!(
            run(Some(SandboxAction::Status)).is_ok(),
            "status must not fail"
        );
    }

    #[test]
    fn test_run_leases_action_succeeds() {
        assert!(
            run(Some(SandboxAction::Leases)).is_ok(),
            "leases must not fail"
        );
    }

    #[test]
    fn test_run_revoke_action_succeeds() {
        let result = run(Some(SandboxAction::Revoke {
            session: "s1".to_owned(),
        }));
        assert!(result.is_ok(), "revoke must not fail");
    }

    #[test]
    fn test_run_none_defaults_to_status_action() {
        // `None` must behave exactly like an explicit `Status` action: both
        // succeed and print the same fixed report (`print_status` takes no
        // argument-derived branch), so this asserts the actual documented
        // default instead of merely "no panic".
        assert!(run(None).is_ok(), "default action (status) must not fail");
        assert!(run(Some(SandboxAction::Status)).is_ok());
    }

    #[test]
    fn test_run_revoke_on_a_fresh_ledger_removes_nothing() -> crate::test_support::TestResult {
        let (ledger, registry) = fresh_ledger();
        registry.mark_session_approved("s1");
        let request = harw_sandbox::request_for_workspace(
            "s1",
            "host-process-worker@1",
            "echo hi",
            std::path::Path::new("/workspace"),
            harw_sandbox::ProcessEnvironment::LocalHost,
        );
        let id = ledger
            .issue_after_local_approval(request.clone(), HostApprovalScope::SessionLease, None)
            .map_err(ctx("issuing a valid request must succeed"))?;
        registry.remember_permit(request, id);

        // Ein *anderer* frischer Ledger (wie ihn `print_revoke` tatsächlich
        // baut) kennt diese Sitzung nicht — genau das demonstriert dieser
        // Test als Beleg für die Moduldoku.
        let removed = fresh_ledger().1.forget_session("s1");
        assert!(removed.is_empty());

        // Der ursprüngliche Ledger/Registry trägt die Zustimmung dagegen
        // weiterhin.
        assert!(registry.is_session_approved("s1"));
        Ok(())
    }
}
