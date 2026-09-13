//! Handler für die Betriebs-Lebenszyklus-Subcommands: `update`, `service`,
//! `catalog`, `uninstall`. Dünne Verdrahtung auf `harw-install`/
//! `harw-model-catalog`; die Logik lebt in den jeweiligen Descriptor-Modulen.

use std::path::{Path, PathBuf};

use harw_install::{Platform, ServiceSpec, UninstallScope, UpdateChecker, detect_service_manager};

use crate::cli::ServiceAction;
use crate::home::resolve_home;

/// Rendert die `harw-install`-Health-Checks (System/Sandbox/Runtime/Service/Home).
pub fn health(home_override: Option<PathBuf>) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let checks = health_checks(&home)?;
    println!("--- Health-Checks ---");
    for (id, outcome) in harw_install::run_all(&checks) {
        let (mark, detail) = match outcome {
            harw_install::CheckOutcome::Ok(detail) => ("ok  ", detail),
            harw_install::CheckOutcome::Warn(detail) => ("warn", detail),
            harw_install::CheckOutcome::Fail(detail) => ("FAIL", detail),
        };
        println!("[{mark}] {id}: {detail}");
    }
    Ok(())
}

/// Builds the install checks with evidence from this CLI's real composition
/// root. `harw-install` deliberately cannot construct or infer this evidence
/// itself because it does not own the registry or session boundary.
fn health_checks(home: &Path) -> Result<Vec<Box<dyn harw_install::DoctorCheck>>, String> {
    let evidence = runtime_composition_evidence(home)?;
    Ok(vec![
        Box::new(harw_install::doctor::SystemCheck),
        Box::new(harw_install::doctor::SandboxCheck),
        Box::new(harw_install::doctor::RuntimeCompositionCheck::new(evidence)),
        Box::new(harw_install::doctor::ServiceManagerCheck),
        Box::new(harw_install::doctor::HomeExistsCheck::new(home)),
        Box::new(harw_install::doctor::HomePermsCheck::new(home)),
    ])
}

/// Observes the tool composition, the trusted spawn context, and the approval
/// boundary that `harw doctor`'s own runtime entry
/// ([`crate::runtime_entry::doctor_assembly`]) actually assembles for `home`.
/// A failed assembly is returned rather than converted into positive or
/// guessed evidence.
fn runtime_composition_evidence(
    home: &Path,
) -> Result<harw_install::doctor::RuntimeCompositionEvidence, String> {
    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    let assembly = crate::runtime_entry::doctor_assembly(home, &cwd)?;
    let rights = assembly.rights_snapshot();
    let has_trusted_spawn_context = assembly.spawn_context().approval_actor.is_some();
    let has_approval_boundary = !rights.approval_chain.is_empty();

    Ok(harw_install::doctor::RuntimeCompositionEvidence {
        advertised_tools: Some(rights.tools),
        has_trusted_spawn_context: Some(has_trusted_spawn_context),
        has_approval_boundary: Some(has_approval_boundary),
    })
}

/// `harw update [--check]`: liest `version.json`.
pub fn update(home_override: Option<PathBuf>, check: bool) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let checker = UpdateChecker::new(&home);

    match checker.read().map_err(|error| error.to_string())? {
        Some(info) => println!(
            "letzte Prüfung: {} · neueste bekannte Version: {}",
            info.last_checked_at, info.latest_version
        ),
        None => println!("noch keine Versionsdaten (version.json fehlt)"),
    }

    if check {
        return Err(
            "Versionsprüfung nicht verfügbar: kein Remote-Update-Checker ist konfiguriert; \
             version.json bleibt unverändert."
                .to_owned(),
        );
    }
    Ok(())
}

/// `harw service <install|status|uninstall>`: verwaltet den Hintergrunddienst.
pub fn service(home_override: Option<PathBuf>, action: ServiceAction) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let platform = Platform::detect();
    let manager = detect_service_manager(&platform);
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;

    let spec = ServiceSpec {
        name: "harw".to_owned(),
        exec: vec![exe.display().to_string(), "gateway".to_owned()],
        working_dir: home.clone(),
        env: vec![("HARW_HOME".to_owned(), home.display().to_string())],
        restart_sec: 5,
    };

    match action {
        ServiceAction::Install { dry_run } => {
            if dry_run {
                println!("{}", manager.render_unit(&spec));
            } else {
                manager.install(&spec).map_err(|error| error.to_string())?;
                println!("Dienst installiert ({:?}).", manager.kind());
            }
        }
        ServiceAction::Status => {
            let status = manager.status("harw").map_err(|error| error.to_string())?;
            println!("Dienststatus: {status:?}");
        }
        ServiceAction::Uninstall => {
            manager
                .uninstall("harw")
                .map_err(|error| error.to_string())?;
            println!("Dienst entfernt.");
        }
    }
    Ok(())
}

/// `harw catalog [--refresh]`: listet den Provider-Katalog.
pub fn catalog(home_override: Option<PathBuf>, refresh: bool) -> Result<(), String> {
    let mut catalog = harw_model_catalog::embedded_catalog();
    let home = resolve_home(home_override)?;
    if refresh {
        harw_model_catalog::models_dev::refresh_models(&home.join("cache"), &mut catalog)
    } else {
        harw_model_catalog::enrich_models(&home.join("cache"), &mut catalog)
    }
    .map_err(|error| error.to_string())?;
    println!("{:<12} {:<28} BASE-URL", "ID", "NAME");
    for provider in &catalog {
        println!(
            "{:<12} {:<28} {}",
            provider.id, provider.name, provider.base_url
        );
    }
    Ok(())
}

/// `harw uninstall --scope … [--dry-run] [--yes]`: entfernt Artefakte.
pub fn uninstall(
    home_override: Option<PathBuf>,
    scope: &[String],
    dry_run: bool,
    yes: bool,
) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let scopes: Vec<UninstallScope> = scope
        .iter()
        .filter_map(|raw| match raw.as_str() {
            "service" => Some(UninstallScope::Service),
            "state" => Some(UninstallScope::State),
            "workspace" => Some(UninstallScope::Workspace),
            "binary" => Some(UninstallScope::Binary),
            _ => None,
        })
        .collect();
    if scopes.is_empty() {
        return Err("kein gültiger --scope (service|state|workspace|binary)".to_owned());
    }

    let plan = harw_install::uninstall::plan(&home, &scopes);
    println!("Zu entfernen:");
    for path in &plan.removals {
        println!("  {}", path.display());
    }
    if !plan.preserved.is_empty() {
        println!("Bleibt erhalten:");
        for path in &plan.preserved {
            println!("  {}", path.display());
        }
    }

    if dry_run {
        println!("(dry-run — nichts entfernt)");
        return Ok(());
    }
    if !yes {
        return Err("Bestätigung nötig: `--yes` zum tatsächlichen Entfernen".to_owned());
    }
    harw_install::uninstall::execute(&plan, false).map_err(|error| error.to_string())?;
    println!("Entfernt.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn health_evidence_observes_default_cli_composition() {
        let home = tempfile::tempdir().expect("create temporary HARW home");

        let evidence =
            runtime_composition_evidence(home.path()).expect("assemble CLI composition");

        // Die Liste ist bewusst vollständig ausgeschrieben und nicht auf eine
        // Mindestmenge geprüft: eine Änderung der Werkzeugfläche der Standard-
        // Zusammenstellung soll auffallen, nicht durchrutschen. `fs.glob` und
        // `fs.grep` kamen mit den Recherche-Werkzeugen hinzu (AP W2-01/02).
        // `rights_snapshot().tools` liefert sortiert und dublettenfrei
        // (harw-runtime/src/assembly.rs `rights_snapshot`), deshalb wird hier
        // als Menge statt per `starts_with`-Präfix verglichen.
        let base: HashSet<&str> = [
            "fs.read",
            "fs.write",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
            "shell.exec",
        ]
        .into_iter()
        .collect();
        let browser: HashSet<&str> = [
            "browser.open",
            "browser.observe",
            "browser.find",
            "browser.act",
            "browser.wait",
            "browser.events",
            "browser.close",
        ]
        .into_iter()
        .collect();
        // Workspace --all-features unifies the registry's optional browser
        // feature into this binary, even though the CLI has no such feature.
        let with_browser: HashSet<&str> = base.union(&browser).copied().collect();

        let advertised = evidence.advertised_tools.expect("advertised tool evidence");
        let advertised: HashSet<&str> = advertised.iter().map(String::as_str).collect();
        assert!(
            advertised == base || advertised == with_browser,
            "unexpected tool surface: {advertised:?}"
        );
        assert_eq!(evidence.has_trusted_spawn_context, Some(true));
        assert_eq!(evidence.has_approval_boundary, Some(true));
    }

    #[test]
    fn update_check_reports_unavailable_without_creating_version_state() {
        let home = tempfile::tempdir().expect("create temporary HARW home");

        let error = update(Some(home.path().to_path_buf()), true)
            .expect_err("update check must fail without a remote checker");

        assert!(error.contains("kein Remote-Update-Checker ist konfiguriert"));
        assert!(!home.path().join("version.json").exists());
    }

    #[test]
    fn update_check_leaves_existing_version_state_unchanged() {
        let home = tempfile::tempdir().expect("create temporary HARW home");
        let checker = UpdateChecker::new(home.path());
        checker
            .write(&harw_install::VersionInfo {
                latest_version: "1.2.3".to_owned(),
                last_checked_at: jiff::Timestamp::now(),
                dismissed_version: Some("1.2.2".to_owned()),
            })
            .expect("write existing version state");
        let version_path = home.path().join("version.json");
        let before = std::fs::read(&version_path).expect("read existing version state");

        let error = update(Some(home.path().to_path_buf()), true)
            .expect_err("update check must fail without a remote checker");

        assert!(error.contains("kein Remote-Update-Checker ist konfiguriert"));
        assert_eq!(
            std::fs::read(version_path).expect("read version state after failed check"),
            before
        );
    }
}
