//! Runtime-Ableitung für den durablen Job-Worker (W2d-1 Welle A, Agent A3;
//! W2d-2 J1, Plan D3b).
//!
//! # Zweck
//! Eine Stelle, an der der Job-Worker Identität, Sandbox und Runtime-Montage
//! eines Jobs aus der gemeinsamen Runtime (`harw-runtime`) ableitet, statt sie
//! in `job_worker.rs` selbst zusammenzusetzen. Vertrag:
//! `docs/remediation/CONTRACTS.md` §principal und §runtime-spec (Zeilen
//! `JobPrompt` / `JobPlanNode`), `docs/remediation/CONTRACTS-W2d2.md` §1.3.
//!
//! # Verantwortung
//! - [`JobEntry`]: welche Art Job läuft (Prompt oder Plan-Knoten) und die
//!   Abbildung auf [`EntryKind`].
//! - [`job_principal`]: der Principal eines Jobs, gebaut an der
//!   vertrauenswürdigen Eingangsgrenze `JobWorker`.
//! - [`job_sandbox`]: die Wurzel-Sandbox eines Jobs — nur über
//!   [`root_sandbox`] bzw. [`plan_node_sandbox`], also nie weiter als die
//!   Profiltabelle. Der Worker nutzt sie als *Prüfung* der Plan-Knoten-Ableitung.
//! - [`JobAssemblyInputs`] / [`job_assembly`]: die Runtime-Montage eines Jobs
//!   direkt über [`RuntimeAssembly::builder`] — mit fester Wurzel-Session-id
//!   und optionaler [`RuntimeNarrowing`] (Plan-Knoten).
//! - [`configured_principal_ids`]: schließt die Restlücke aus
//!   `docs/remediation/ledger/W1/W1-13.md` §1 — der Worker prüft, ob der
//!   Einreicher eines Jobs noch ein *aktuell konfigurierter* MCP-Principal ist.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind synchron und zustandslos; geteilte Speicher werden als
//! `Arc` hereingereicht und nur weitergegeben. [`job_assembly`] liest
//! Konfiguration und Projekt vom Dateisystem.
//!
//! # Fehler
//! Fehler erscheinen als `String`; Sandbox- und Montagefehler sind die
//! `Display`-Form von `harw_runtime::RuntimeError`.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use harw_authority::SandboxSpec;
use harw_config::ResolvedConfig;
use harw_core::{ModelProvider, StateStore};
use harw_plan::PlanNodeKind;
use harw_runtime::{
    EntryKind, ModelSource, RuntimeAssembly, RuntimeNarrowing, RuntimeStores, plan_node_sandbox,
    root_sandbox,
};
use harw_session_store::JobStore;
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind, SessionId};

/// Art eines durablen Jobs aus Sicht der Runtime.
///
/// # Description
/// `Prompt` ist ein MCP-eingereichter Prompt-Job (Profil `JobPrompt`: keine
/// Rechte, keine Werkzeuge). `PlanNode` ist ein Plan-Knoten-Job (Profil
/// `JobPlanNode`: höchstens `{Read, Write}`); `may_write` ist das Ergebnis der
/// Vertragsableitung im Worker (`derive_plan_node_sandbox`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobEntry {
    /// Prompt-Job ohne Werkzeuge und ohne Rechte.
    Prompt,
    /// Plan-Knoten-Job.
    PlanNode {
        /// Art des Plan-Knotens.
        kind: PlanNodeKind,
        /// Ob der Knotenvertrag Schreibrecht trägt.
        may_write: bool,
    },
}

impl JobEntry {
    /// Bildet die Job-Art auf den Runtime-Einstieg ab (CONTRACTS.md §runtime-spec).
    ///
    /// # Returns
    /// `Prompt` → [`EntryKind::JobPrompt`], `PlanNode` → [`EntryKind::JobPlanNode`].
    #[must_use]
    pub(crate) fn entry_kind(self) -> EntryKind {
        match self {
            JobEntry::Prompt => EntryKind::JobPrompt,
            JobEntry::PlanNode { .. } => EntryKind::JobPlanNode,
        }
    }
}

/// Baut den Principal eines Jobs an der Eingangsgrenze des Job-Workers.
///
/// # Description
/// `Channel` × `JobWorker` × `Observer` (CONTRACTS.md §principal, R7). Die
/// Kennung ist die id des authentifizierten Einreichers aus dem gespeicherten
/// Job-Scope, nie ein Wert aus der Job-Eingabe. Auf `JobWorker` liefert
/// [`Principal::approval_actor`] bewusst `None`: ein Job beantwortet keine
/// Freigaben selbst.
///
/// Der Tier ist auf `Observer` gesetzt, nicht weil ein Job nur lesen dürfte,
/// sondern weil er auf `JobWorker`-Einstiegen (`EntryKind::JobPrompt`,
/// `EntryKind::JobPlanNode`) wirkungslos ist: keiner der beiden Bau-Pfade ruft
/// `harw_runtime::permissions_for_tier`, das nur der Web-Einstieg nutzt
/// (`harw-cli/src/runtime_web.rs`), und `RuntimeAssembly::build` liest
/// `principal.tier()` an keiner Stelle. Rechte kommen für Job-Einstiege
/// ausschließlich aus der Profiltabelle bzw. `RuntimeNarrowing::permissions`
/// (R0-F). Der niedrigste Tier macht die Absicht sichtbar: ein Job-Principal
/// trägt selbst keine Mutationsbefugnis, die Mutation kommt aus der
/// Vertragsableitung.
///
/// # Arguments
/// - `submitter_id`: Operator-id des Einreichers (`ApprovalActor::Operator { id }`).
#[must_use]
pub(crate) fn job_principal(submitter_id: &str) -> Principal {
    Principal::trusted_ingress(
        PrincipalKind::Channel,
        submitter_id,
        IngressSurface::JobWorker,
        PermissionTier::Observer,
    )
}

/// Baut die Wurzel-Sandbox eines Jobs (CONTRACTS.md §runtime-spec).
///
/// # Description
/// `Prompt` → [`root_sandbox`] mit [`EntryKind::JobPrompt`] (leere Rechte);
/// `PlanNode` → [`plan_node_sandbox`], das auf `{Read}` bzw. `{Read, Write}`
/// schneidet. `ExecuteProcess` und Netz entstehen auf keinem Weg.
///
/// # Arguments
/// - `entry`: Job-Art.
/// - `project_root`: Wurzel des Projekts (muss ein existierendes Verzeichnis sein).
///
/// # Errors
/// Die `Display`-Form von `RuntimeError::Sandbox`, wenn `project_root` nicht
/// kanonisierbar ist, kein Verzeichnis ist oder die Registrierung scheitert.
pub(crate) fn job_sandbox(entry: JobEntry, project_root: &Path) -> Result<SandboxSpec, String> {
    match entry {
        JobEntry::Prompt => root_sandbox(EntryKind::JobPrompt, project_root),
        JobEntry::PlanNode { kind, may_write } => plan_node_sandbox(kind, may_write, project_root),
    }
    .map_err(|error| error.to_string())
}

/// Eingaben der Runtime-Montage eines Jobs (CONTRACTS-W2d2.md §1.3).
///
/// # Description
/// Bündelt alles, was [`job_assembly`] braucht, damit die Montage genau eine
/// Aufrufstelle mit benannten Feldern hat (statt einer langen Parameterliste).
pub(crate) struct JobAssemblyInputs<'a> {
    /// Job-Art; bestimmt den Einstieg ([`JobEntry::entry_kind`]).
    pub(crate) entry: JobEntry,
    /// Harness-Home (`--home` bzw. `HARW_HOME`).
    pub(crate) home: &'a Path,
    /// Arbeitsverzeichnis des Jobs (Projekterkennung der Montage).
    pub(crate) cwd: &'a Path,
    /// Principal aus [`job_principal`].
    pub(crate) principal: Principal,
    /// Wurzel-Session-id; der Worker übergibt `durable-job-<work id>`.
    pub(crate) session_id: SessionId,
    /// Durabler Verlaufsspeicher des Jobs.
    pub(crate) state_store: Arc<dyn StateStore>,
    /// Job-Speicher des Workers.
    pub(crate) job_store: Arc<JobStore>,
    /// Provider des Jobs (ggf. budgetiert); wird unverändert übernommen.
    pub(crate) model: Arc<dyn ModelProvider>,
    /// Verengung von Profil, Identität, Rechten und Workspace-Root (R0-F); `Some` nur für
    /// Plan-Knoten.
    pub(crate) narrowing: Option<RuntimeNarrowing>,
}

/// Montiert die Runtime eines Jobs direkt über [`RuntimeAssembly::builder`].
///
/// # Description
/// Spec: [`crate::runtime_entry::runtime_spec`] mit dem Einstieg der Job-Art.
/// Speicher: `state_store`, `job_store: Some(..)`, keine durablen Freigaben.
/// Modell: der vom Worker bereits gebaute Provider ([`ModelSource::Override`]).
/// Wurzel-Session-id: fest `inputs.session_id` — der Worker muss dieselbe id an
/// `RuntimeAssembly::new_root_session` geben. Verengung: nur wenn
/// `inputs.narrowing` gesetzt ist (Bau-Semantik fail-closed in `harw-runtime`).
/// Keine `session_events`: beide Job-Profile haben `SpawnerPolicy::None`
/// (`harw-runtime/src/spec.rs`), der Bau verlangt Events nur für
/// `SpawnerPolicy::BuiltinRoles`.
///
/// # Arguments
/// - `inputs` ([`JobAssemblyInputs`]): alle Montage-Eingaben; wird verbraucht.
///
/// # Returns
/// Die montierte [`RuntimeAssembly`].
///
/// # Errors
/// Die `Display`-Form jedes `RuntimeError` des Baus (Konfiguration, Vertrauen,
/// Projekterkennung, Sandbox, Registry/Verengung, Provider, Spawner).
///
/// # Concurrency
/// Synchron; liest Konfiguration und Projekt vom Dateisystem.
pub(crate) fn job_assembly(inputs: JobAssemblyInputs<'_>) -> Result<RuntimeAssembly, String> {
    let JobAssemblyInputs {
        entry,
        home,
        cwd,
        principal,
        session_id,
        state_store,
        job_store,
        model,
        narrowing,
    } = inputs;
    let spec = crate::runtime_entry::runtime_spec(entry.entry_kind(), home, cwd, principal);
    let stores = RuntimeStores {
        state_store,
        job_store: Some(job_store),
        approval_store: None,
    };
    let mut builder = RuntimeAssembly::builder(spec)
        .model(ModelSource::Override(model))
        .stores(stores)
        .root_session_id(session_id);
    if let Some(narrowing) = narrowing {
        builder = builder.narrowing(narrowing);
    }
    builder.build().map_err(|error| error.to_string())
}

/// Sammelt die ids aller konfigurierten MCP-Principals.
///
/// # Description
/// Quelle: `config.harness.mcp_listener.principals[].id`
/// (`harw-config/src/harness_config.rs`, `McpListenerSection::principals`,
/// `McpPrincipalToml::id`). Unabhängig von `mcp_listener.enabled`: auch ein
/// abgeschalteter Listener definiert, welche Einreicher bekannt sind.
///
/// # Returns
/// Menge der ids, dedupliziert und sortiert.
#[must_use]
pub(crate) fn configured_principal_ids(config: &ResolvedConfig) -> BTreeSet<String> {
    config
        .harness
        .mcp_listener
        .principals
        .iter()
        .map(|principal| principal.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{TestResult, ctx};
    use harw_authority::Permission;
    use harw_config::{HarnessConfig, McpListenerSection, McpPrincipalToml, SecretRef};

    fn listener_principal(id: &str) -> McpPrincipalToml {
        McpPrincipalToml {
            id: id.to_owned(),
            credential_ref: SecretRef::Env("HARW_TEST_MCP_TOKEN".to_owned()),
            tenant: "tenant-a".to_owned(),
            workspace: "workspace-a".to_owned(),
            job_capabilities: Vec::new(),
        }
    }

    fn config_with_principals(ids: &[&str]) -> ResolvedConfig {
        ResolvedConfig {
            harness: HarnessConfig {
                mcp_listener: McpListenerSection {
                    principals: ids.iter().copied().map(listener_principal).collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_job_entry_kind_maps_prompt_and_plan_node() {
        assert_eq!(JobEntry::Prompt.entry_kind(), EntryKind::JobPrompt);
        for may_write in [false, true] {
            let entry = JobEntry::PlanNode {
                kind: PlanNodeKind::Coding,
                may_write,
            };
            assert_eq!(entry.entry_kind(), EntryKind::JobPlanNode);
        }
    }

    #[test]
    fn test_job_principal_uses_observer_tier() {
        let principal = job_principal("client-7");
        assert_eq!(principal.kind(), PrincipalKind::Channel);
        assert_eq!(principal.id(), "client-7");
        assert_eq!(principal.surface(), IngressSurface::JobWorker);
        assert_eq!(principal.tier(), PermissionTier::Observer);
        assert_eq!(principal.approval_actor(), None);
    }

    #[test]
    fn test_job_sandbox_prompt_has_no_permissions() -> TestResult {
        // tempfile ist dev-dependency von harw-cli; root_sandbox kanonisiert den Pfad.
        let dir = tempfile::tempdir().map_err(ctx("test: tempdir"))?;
        let sandbox =
            job_sandbox(JobEntry::Prompt, dir.path()).map_err(ctx("test: sandbox binds"))?;
        assert_eq!(sandbox.permissions().iter().count(), 0);
        assert!(sandbox.network_scope().is_empty());
        Ok(())
    }

    #[test]
    fn test_job_sandbox_research_node_never_writes_or_executes() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("test: tempdir"))?;
        for may_write in [false, true] {
            let entry = JobEntry::PlanNode {
                kind: PlanNodeKind::Research,
                may_write,
            };
            let sandbox = job_sandbox(entry, dir.path()).map_err(ctx("test: sandbox binds"))?;
            let permissions = sandbox.permissions();
            assert!(permissions.contains(Permission::ReadWorkspace));
            assert!(!permissions.contains(Permission::WriteWorkspace));
            assert!(!permissions.contains(Permission::ExecuteProcess));
            assert!(!permissions.contains(Permission::NetworkAccess));
            assert!(sandbox.network_scope().is_empty());
        }
        Ok(())
    }

    #[test]
    fn test_configured_principal_ids_collects_listener_ids() {
        let config = config_with_principals(&["beta", "alpha", "beta"]);
        let ids = configured_principal_ids(&config);
        let expected: BTreeSet<String> = ["alpha", "beta"].into_iter().map(str::to_owned).collect();
        assert_eq!(ids, expected);
        assert!(configured_principal_ids(&ResolvedConfig::default()).is_empty());
    }

    #[test]
    fn test_job_assembly_prompt_uses_given_session_id_and_no_tools() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("test: home tempdir"))?;
        let cwd = tempfile::tempdir().map_err(ctx("test: cwd tempdir"))?;
        let jobs = tempfile::tempdir().map_err(ctx("test: job store tempdir"))?;
        harw_home::ensure_home(home.path()).map_err(ctx("test: scaffold home"))?;
        let session_id = SessionId::from_str("durable-job-assembly-test");

        let assembly = job_assembly(JobAssemblyInputs {
            entry: JobEntry::Prompt,
            home: home.path(),
            cwd: cwd.path(),
            principal: job_principal("client-7"),
            session_id: session_id.clone(),
            state_store: Arc::new(harw_core::InMemoryStateStore::new()),
            job_store: Arc::new(JobStore::new(jobs.path())),
            model: Arc::new(harw_core::EchoModelProvider::new("x")),
            narrowing: None,
        })
        .map_err(ctx("test: prompt job assembly builds"))?;

        assert_eq!(assembly.root_session_id(), &session_id);
        assert_eq!(assembly.spec().entry, EntryKind::JobPrompt);
        assert!(assembly.rights_snapshot().tools.is_empty());
        assert_eq!(assembly.sandbox().permissions().iter().count(), 0);
        assert!(assembly.job_store().is_some());
        assert_eq!(assembly.spawn_context().approval_actor, None);
        // R7: a `JobPrompt` assembly has no operations surface, so the
        // principal's `PermissionTier` (now `Observer`, see `job_principal`)
        // has nothing to gate here.
        assert_eq!(assembly.operations().iter().count(), 0);
        Ok(())
    }
}
