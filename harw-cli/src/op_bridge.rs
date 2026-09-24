//! Brücke von CLI-Unterbefehlen zu Slash-Operationen.
//!
//! # Beschreibung
//! Einige `harw`-Befehle (Jobs, Gedächtnis, Skills, Plugins, Vorschläge …)
//! gibt es als Slash-Befehl im Chat bereits. Statt ihre Logik ein zweites
//! Mal zu schreiben, führt [`run_operation`] die gleichnamige Operation
//! außerhalb einer Chat-Sitzung aus:
//!
//! 1. Eine lokale [`RuntimeSpec`] für [`EntryKind::Analyze`] bauen — die
//!    Einstiegsart mit reiner Befehlsfläche (`OperationSurface::CommandsOnly`)
//!    und ohne Rückfragen (`AskResolution::Fail`).
//! 2. Die [`RuntimeAssembly`] mit einem nie aufgerufenen Echo-Modell, einem
//!    flüchtigen Verlaufsspeicher, dem Job-Speicher des aktiven Profils, den
//!    Plan-Diensten (falls eingeschaltet) und dem Gedächtnis des aktiven
//!    Profils montieren.
//! 3. Die Operation über ihren Slash-Pfad ([`CommandAdapter`]) finden, die
//!    Berechtigungsstufe prüfen und auf einer Single-Thread-Tokio-Runtime
//!    ausführen.
//!
//! Befehle, die eine laufende Chat-Sitzung verändern oder beschreiben
//! (`/status`, `/usage`, `/new`, `/compact`, `/mode`), lehnt die Brücke ab:
//! außerhalb einer Sitzung haben sie keinen Gegenstand.
//!
//! # Nebenläufigkeit
//! Synchron; baut je Aufruf eine eigene Single-Thread-Runtime. Nicht aus
//! einer laufenden Tokio-Runtime heraus aufrufen.
//!
//! # Fehler
//! Alle Fehler sind deutsche `String`s ohne Geheimnisse.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_agent_dsl::roles::AgentRoleId;
use harw_config::ResolvedConfig;
use harw_core::InMemoryStateStore;
use harw_operations::OpError;
use harw_operations::OpOutput;
use harw_operations::adapter::CommandAdapter;
use harw_runtime::{
    EntryKind, ModelSource, RuntimeAssembly, RuntimeSpec, RuntimeStores, ServiceSurface,
};
use harw_session_store::JobStore;
use harw_types::{IngressSurface, TurnId};

use crate::runtime_entry::{local_principal, runtime_spec};

/// Slash-Befehle, die nur innerhalb einer laufenden Chat-Sitzung Sinn haben.
const SESSION_BOUND_COMMANDS: [&str; 5] = ["/status", "/usage", "/new", "/compact", "/mode"];

/// Antwort des nie aufgerufenen Echo-Modells dieser Montage.
const ECHO_REPLY: &str = "harw cli operation bridge";

/// Wo eine Operation laufen soll.
#[derive(Clone, Debug)]
pub(crate) struct OpTarget<'a> {
    /// Expliziter Root-Space (`--home`); `None` heißt Standardauflösung.
    pub home: Option<PathBuf>,
    /// Arbeitsverzeichnis (bestimmt Projekt und Sandbox).
    pub cwd: &'a Path,
}

/// Führt eine Slash-Operation außerhalb einer Chat-Sitzung aus.
///
/// # Argumente
/// - `target` ([`OpTarget`]): Root-Space und Arbeitsverzeichnis.
/// - `path` (`&str`): der Slash-Pfad, mit oder ohne führendes `/`
///   (z. B. `"/memory"` oder `"memory"`).
/// - `args` (`Vec<String>`): die Argumente, wie sie im Chat hinter dem
///   Befehl stünden.
///
/// # Rückgabe
/// Das [`OpOutput`] der Operation; die Ausgabe übernimmt der Aufrufer
/// (z. B. über `crate::output::Printer::op_output`).
///
/// # Fehler
/// `Err(String)`, wenn der Pfad leer oder sitzungsgebunden ist, Home,
/// Konfiguration oder Montage scheitern, die Operation hier nicht
/// registriert ist, die Berechtigungsstufe nicht reicht oder die Operation
/// selbst einen Fehler meldet.
pub(crate) fn run_operation(
    target: OpTarget<'_>,
    path: &str,
    args: Vec<String>,
) -> Result<OpOutput, String> {
    let command = normalize_command_path(path)?;
    refuse_session_bound(&command)?;

    let home = crate::home::resolve_home(target.home)?;
    crate::home::ensure_home(&home)?;
    let spec = runtime_spec(
        EntryKind::Analyze,
        &home,
        target.cwd,
        local_principal(IngressSurface::Cli),
    );
    let (config, _trust) = harw_runtime::load_config(&spec).map_err(|error| error.to_string())?;

    // Die Assembly braucht den Ereigniskanal für `SpawnerPolicy::BuiltinRoles`;
    // der Empfänger bleibt bis zum Ende dieser Funktion gebunden, damit keine
    // Sendung an einem geschlossenen Kanal endet.
    let (assembly, _event_rx) = bridge_assembly(spec, &home, &config)?;

    let adapter = find_command(&assembly, &command)?;
    let required = adapter.permission();
    let actual = assembly.principal().tier();
    if actual < required {
        return Err(format!(
            "`{command}` erfordert mindestens Berechtigungsstufe {required:?}, \
             der aktuelle Benutzer hat nur {actual:?}"
        ));
    }

    let ctx = assembly.op_context(
        ServiceSurface::Slash,
        assembly.root_session_id().clone(),
        TurnId::new(),
        assembly.sandbox().clone(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Laufzeit konnte nicht gestartet werden: {error}"))?;
    runtime
        .block_on(adapter.dispatch(&ctx, args))
        .map_err(|error| op_error_message(&command, &error))
}

/// Montiert die Runtime der Brücke.
///
/// # Rückgabe
/// Die Assembly und den Empfänger ihres Sitzungs-Ereigniskanals.
fn bridge_assembly(
    spec: RuntimeSpec,
    home: &Path,
    config: &ResolvedConfig,
) -> Result<
    (
        RuntimeAssembly,
        tokio::sync::mpsc::UnboundedReceiver<harw_protocol::SessionEvent>,
    ),
    String,
> {
    let job_store = Arc::new(JobStore::new(&profile_storage_root(home)?));
    let plan = open_plan_services(&spec.cwd, config)?;
    let memory = open_memory(home, config);

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut builder = RuntimeAssembly::builder(spec)
        .model(ModelSource::Echo(ECHO_REPLY.to_owned()))
        .stores(RuntimeStores {
            state_store: Arc::new(InMemoryStateStore::new()),
            job_store: Some(job_store),
            approval_store: None,
        })
        .session_events(event_tx);
    if let Some(plan) = plan {
        builder = builder.plan_services(plan);
    }
    if let Some(memory) = memory {
        builder = builder.memory(memory);
    }
    let assembly = builder
        .build()
        .map_err(|error| format!("Laufzeit konnte nicht aufgebaut werden: {error}"))?;
    Ok((assembly, event_rx))
}

/// Sucht den Slash-Befehl `command` unter den Operationen der Montage.
fn find_command(assembly: &RuntimeAssembly, command: &str) -> Result<CommandAdapter, String> {
    assembly
        .operations()
        .iter()
        .flat_map(|operation| CommandAdapter::from_operation(Arc::clone(operation)))
        .find(|adapter| adapter.path() == command)
        .ok_or_else(|| format!("`{command}` ist hier nicht verfügbar"))
}

/// Normalisiert einen Befehlspfad auf die Form `/name`.
///
/// # Fehler
/// `Err(String)` bei leerem Pfad oder Leerzeichen im Pfad.
fn normalize_command_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim();
    let name = trimmed.strip_prefix('/').unwrap_or(trimmed);
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err(format!("ungültiger Befehlsname: `{path}`"));
    }
    Ok(format!("/{name}"))
}

/// Lehnt Befehle ab, die nur in einer laufenden Chat-Sitzung Sinn haben.
fn refuse_session_bound(command: &str) -> Result<(), String> {
    if SESSION_BOUND_COMMANDS.contains(&command) {
        return Err(format!(
            "`{command}` bezieht sich auf eine laufende Chat-Sitzung und ist nur im Chat \
             verfügbar (`harw chat`)"
        ));
    }
    Ok(())
}

/// Übersetzt einen Operationsfehler in eine deutsche Meldung.
fn op_error_message(command: &str, error: &OpError) -> String {
    match error {
        OpError::InvalidArguments(message) => {
            format!("ungültige Argumente für `{command}`: {message}")
        }
        OpError::Execution(message) => format!("`{command}` ist fehlgeschlagen: {message}"),
        OpError::NotAvailable(message) => {
            format!("`{command}` ist hier nicht verfügbar: {message}")
        }
    }
}

/// Ordner des aktiven Profils; [`JobStore::new`] hängt selbst `jobs` an.
fn profile_storage_root(home: &Path) -> Result<PathBuf, String> {
    let profile = harw_home::active_profile_name(home);
    harw_home::profile_dir(home, &profile)
        .map_err(|error| format!("Profilverzeichnis für '{profile}' nicht auflösbar: {error}"))
}

/// Öffnet die Plan-Dienste, falls die Planungsfläche eingeschaltet ist.
///
/// # Fehler
/// `Err(String)` bei ungültiger `[tools.plan]`-Konfiguration, fehlgeschlagener
/// Projekterkennung oder nicht öffenbarem Speicher.
fn open_plan_services(
    cwd: &Path,
    config: &ResolvedConfig,
) -> Result<Option<harw_runtime::PlanServices>, String> {
    let plan_config = crate::plan_tool_config_from_section(&config.harness.tools.plan)?;
    if !plan_config.is_enabled() {
        return Ok(None);
    }
    let project =
        harw_home::project::discover_project(cwd, &[]).map_err(|error| error.to_string())?;
    let project_home = harw_home::project::ProjectHome::at(&project);
    let services = crate::build_plan_services(
        &project_home,
        &plan_config,
        crate::DEFAULT_PLAN_SPACE,
        crate::DEFAULT_GOAL_SPACE,
    )?;
    Ok(services.to_runtime())
}

/// Öffnet das Gedächtnis wie der Chat: das der aktiven UIA, sonst das des
/// aktiven Profils. Scheitert das Öffnen, bleibt es aus (die Operation meldet
/// dann selbst, dass kein Gedächtnis verfügbar ist).
fn open_memory(home: &Path, config: &ResolvedConfig) -> Option<Arc<dyn harw_memory::Memory>> {
    let root = match uia_memory_root(config) {
        Some(root) => root,
        None => match profile_storage_root(home) {
            Ok(profile) => profile.join("memories"),
            Err(error) => {
                tracing::warn!(%error, "op_bridge: Profilverzeichnis für Gedächtnis fehlt");
                return None;
            }
        },
    };
    match harw_memory::FileMemoryStore::open(&root) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            tracing::warn!(path = %root.display(), %error, "op_bridge: Gedächtnis nicht öffenbar");
            None
        }
    }
}

/// `<agent_dir>/memory`, wenn eine aktive UIA (Rolle `user-interface`)
/// konfiguriert ist — dieselbe Regel wie beim Chat-Start.
fn uia_memory_root(config: &ResolvedConfig) -> Option<PathBuf> {
    let definition_id = config.harness.active_uia_definition.as_deref()?;
    let is_uia = config
        .executable_agents
        .get(definition_id)
        .is_some_and(|ir| ir.role() == AgentRoleId::UserInterface);
    if !is_uia {
        return None;
    }
    let agent_dir = config.agent_definition_dirs.get(definition_id)?;
    Some(agent_dir.join("memory"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use tempfile::TempDir;

    #[test]
    fn normalize_command_path_adds_a_leading_slash() {
        assert_eq!(normalize_command_path("memory"), Ok("/memory".to_owned()));
        assert_eq!(normalize_command_path("/skills"), Ok("/skills".to_owned()));
        assert_eq!(normalize_command_path(" jobs "), Ok("/jobs".to_owned()));
    }

    #[test]
    fn normalize_command_path_rejects_empty_and_spaced_names() {
        assert!(normalize_command_path("").is_err());
        assert!(normalize_command_path("/").is_err());
        assert!(normalize_command_path("a b").is_err());
    }

    #[test]
    fn session_bound_commands_are_refused_in_german() -> TestResult {
        for command in SESSION_BOUND_COMMANDS {
            let error = refuse_session_bound(command)
                .err()
                .ok_or(TestError::Missing("session-bound command must be refused"))?;
            assert!(error.contains("laufende Chat-Sitzung"), "{error}");
        }
        assert_eq!(refuse_session_bound("/memory"), Ok(()));
        Ok(())
    }

    #[test]
    fn op_errors_map_to_german_messages() {
        assert_eq!(
            op_error_message("/jobs", &OpError::InvalidArguments("x".to_owned())),
            "ungültige Argumente für `/jobs`: x"
        );
        assert_eq!(
            op_error_message("/jobs", &OpError::Execution("y".to_owned())),
            "`/jobs` ist fehlgeschlagen: y"
        );
        assert_eq!(
            op_error_message("/jobs", &OpError::NotAvailable("z".to_owned())),
            "`/jobs` ist hier nicht verfügbar: z"
        );
    }

    #[test]
    fn run_operation_refuses_session_bound_before_touching_home() -> TestResult {
        let cwd = TempDir::new().map_err(ctx("cwd tempdir"))?;
        // Ein nicht existierendes Home: die Ablehnung muss vorher greifen.
        let target = OpTarget {
            home: Some(cwd.path().join("does-not-exist")),
            cwd: cwd.path(),
        };

        let error = run_operation(target, "status", Vec::new())
            .err()
            .ok_or(TestError::Missing("/status must be refused"))?;

        assert!(error.contains("`/status`"), "{error}");
        assert!(!cwd.path().join("does-not-exist").exists());
        Ok(())
    }

    #[test]
    fn run_operation_reports_unknown_commands() -> TestResult {
        let home = TempDir::new().map_err(ctx("home tempdir"))?;
        let cwd = TempDir::new().map_err(ctx("cwd tempdir"))?;
        let target = OpTarget {
            home: Some(home.path().to_path_buf()),
            cwd: cwd.path(),
        };

        let error = run_operation(target, "gibt-es-nicht", Vec::new())
            .err()
            .ok_or(TestError::Missing("unknown command must fail"))?;

        assert!(error.contains("/gibt-es-nicht"), "{error}");
        Ok(())
    }
}
