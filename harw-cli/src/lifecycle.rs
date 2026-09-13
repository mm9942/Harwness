//! Handler für die Betriebs-Lebenszyklus-Subcommands: `update`, `service`,
//! `catalog`, `uninstall`. Dünne Verdrahtung auf `harw-install`/
//! `harw-model-catalog`; die Logik lebt in den jeweiligen Descriptor-Modulen.

use std::path::PathBuf;

use harw_core::SpawnContext;
use harw_install::{Platform, ServiceSpec, UninstallScope, UpdateChecker, detect_service_manager};
use harw_observe::TraceContext;
use harw_sandbox::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_types::{ApprovalActor, TenantId, WorkspaceId};
use uuid::Uuid;

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
fn health_checks(
    home: &std::path::Path,
) -> Result<Vec<Box<dyn harw_install::DoctorCheck>>, String> {
    let evidence = runtime_composition_evidence()?;
    Ok(vec![
        Box::new(harw_install::doctor::SystemCheck),
        Box::new(harw_install::doctor::SandboxCheck),
        Box::new(harw_install::doctor::RuntimeCompositionCheck::new(evidence)),
        Box::new(harw_install::doctor::ServiceManagerCheck),
        Box::new(harw_install::doctor::HomeExistsCheck::new(home)),
        Box::new(harw_install::doctor::HomePermsCheck::new(home)),
    ])
}

/// Observes the default registry, the trusted spawn context, and the approval
/// handler assembled for the same CLI project path. A failed assembly is
/// returned rather than converted into positive or guessed evidence.
fn runtime_composition_evidence() -> Result<harw_install::doctor::RuntimeCompositionEvidence, String>
{
    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    let assembled = harw_registry_defaults::assemble_default_registry(cwd)
        .map_err(|error| error.to_string())?;
    let advertised_tools = assembled
        .registry
        .tool_providers()
        .iter()
        .flat_map(|provider| provider.tools())
        .map(|tool| tool.name().to_owned())
        .collect();
    build_doctor_spawn_context(&assembled.project.project_root)?;

    Ok(harw_install::doctor::RuntimeCompositionEvidence {
        advertised_tools: Some(advertised_tools),
        has_trusted_spawn_context: Some(true),
        has_approval_boundary: Some(assembled.registry.approval_handlers().len() == 1),
    })
}

/// Generates a fresh root trace for one `harw doctor` run.
///
/// This call site is a root: a doctor invocation has no parent whose trace it
/// could inherit, so the `trace_id` that ties together this run's evidence
/// gathering originates here. Mirrors `harw-core`'s `new_span_id` random
/// source (`harw-core/src/child_controller.rs`) instead of inventing a second
/// one: `uuid::Uuid::new_v4` supplies the full 32 hex characters for
/// `trace_id`, a second, independent draw supplies the first 16 for
/// `span_id`. Both are already valid lowercase hex of the required length by
/// construction; [`TraceContext::new`] still validates rather than setting
/// fields directly.
fn new_doctor_root_trace() -> Result<TraceContext, String> {
    let trace_id = Uuid::new_v4().simple().to_string();
    let span_id = Uuid::new_v4().simple().to_string()[..16].to_owned();
    TraceContext::new(trace_id, span_id)
        .map_err(|error| format!("could not build doctor root trace context: {error}"))
}

/// Constructs the same trusted sandbox/approval binding used by one-shot CLI
/// turns, so the doctor result reflects an observed `SpawnContext` rather than
/// a structural assumption about the registry.
fn build_doctor_spawn_context(project_root: &std::path::Path) -> Result<SpawnContext, String> {
    let tenant = TenantId::from_str("cli");
    let workspace = WorkspaceId::from_str("project");
    let registry = WorkspaceRegistry::build(
        project_root,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: PathBuf::from("."),
        }],
    )
    .map_err(|error| error.to_string())?;
    let binding = registry
        .resolve(&tenant, &workspace)
        .map_err(|error| error.to_string())?;
    let organizational_role = serde_json::from_str("\"root-orchestrator\"")
        .map_err(|error| format!("could not resolve doctor organizational role: {error}"))?;
    let trace = new_doctor_root_trace()?;

    Ok(SpawnContext {
        sandbox: SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
            ]),
        ),
        suggestions: None,
        capability_snapshot: None,
        approval_actor: Some(ApprovalActor::Operator {
            id: "local-cli".to_owned(),
        }),
        organizational_role,
        // Root: no parent exists whose trace could be inherited — see
        // `new_doctor_root_trace`.
        trace: Some(trace),
        // Root: no parent exists whose already-cut ceiling could be
        // inherited, so the ceiling is created here, once — see
        // `crate::root_context::local_root_context_ceiling`.
        ceiling: Some(crate::root_context::local_root_context_ceiling()),
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
    use super::*;

    #[test]
    fn health_evidence_observes_default_cli_composition() {
        let evidence = runtime_composition_evidence().expect("assemble CLI composition");

        // Die Liste ist bewusst vollständig ausgeschrieben und nicht auf eine
        // Mindestmenge geprüft: eine Änderung der Werkzeugfläche der Standard-
        // Zusammenstellung soll auffallen, nicht durchrutschen. `fs.glob` und
        // `fs.grep` kamen mit den Recherche-Werkzeugen hinzu (AP W2-01/02).
        let base = [
            "fs.read",
            "fs.write",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
            "shell.exec",
        ];
        let browser = [
            "browser.open",
            "browser.observe",
            "browser.find",
            "browser.act",
            "browser.wait",
            "browser.events",
            "browser.close",
        ];
        let advertised = evidence.advertised_tools.expect("advertised tool evidence");
        let advertised = advertised.iter().map(String::as_str).collect::<Vec<_>>();
        assert!(advertised.starts_with(&base));
        // Workspace --all-features unifies the registry's optional browser
        // feature into this binary, even though the CLI has no such feature.
        let optional = &advertised[base.len()..];
        assert!(
            optional.is_empty() || optional == browser,
            "unexpected optional tool surface: {optional:?}"
        );
        assert_eq!(evidence.has_trusted_spawn_context, Some(true));
        assert_eq!(evidence.has_approval_boundary, Some(true));
    }

    /// AW1-01c: the doctor spawn context is a root — it carries a
    /// freshly-generated trace with the right hex shapes and no parent span.
    #[test]
    fn doctor_context_carries_a_freshly_generated_root_trace() {
        let project = tempfile::tempdir().expect("create project directory");
        let context = build_doctor_spawn_context(project.path()).expect("build spawn context");

        let trace = context.trace.expect("doctor root must carry a trace");
        assert_eq!(trace.trace_id.len(), 32);
        assert!(trace.trace_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(trace.trace_id, trace.trace_id.to_lowercase());
        assert_eq!(trace.span_id.len(), 16);
        assert!(trace.span_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(trace.span_id, trace.span_id.to_lowercase());
        assert!(
            trace.parent_span_id.is_none(),
            "a root trace must not carry a parent span"
        );
    }

    /// AW1-01c: two doctor runs must not look like the same run — the random
    /// source must not be broken/constant.
    #[test]
    fn doctor_context_root_traces_differ_across_two_calls() {
        let project = tempfile::tempdir().expect("create project directory");
        let first = build_doctor_spawn_context(project.path())
            .expect("build first spawn context")
            .trace
            .expect("first call must carry a trace");
        let second = build_doctor_spawn_context(project.path())
            .expect("build second spawn context")
            .trace
            .expect("second call must carry a trace");

        assert_ne!(first.trace_id, second.trace_id);
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
