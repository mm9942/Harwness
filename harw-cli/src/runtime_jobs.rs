//! Runtime-Ableitung für den durablen Job-Worker (W2d-1 Welle A, Agent A3).
//!
//! # Zweck
//! Eine Stelle, an der der Job-Worker Identität, Sandbox und Runtime-Montage
//! eines Jobs aus der gemeinsamen Runtime (`harw-runtime`) ableitet, statt sie
//! in `job_worker.rs` selbst zusammenzusetzen. Vertrag:
//! `docs/remediation/CONTRACTS.md` §principal und §runtime-spec (Zeilen
//! `JobPrompt` / `JobPlanNode`).
//!
//! # Verantwortung
//! - [`JobEntry`]: welche Art Job läuft (Prompt oder Plan-Knoten) und die
//!   Abbildung auf [`EntryKind`].
//! - [`job_principal`]: der Principal eines Jobs, gebaut an der
//!   vertrauenswürdigen Eingangsgrenze `JobWorker`.
//! - [`job_sandbox`]: die Wurzel-Sandbox eines Jobs — nur über
//!   [`root_sandbox`] bzw. [`plan_node_sandbox`], also nie weiter als die
//!   Profiltabelle.
//! - [`job_assembly`]: die Runtime-Montage eines Jobs über
//!   `crate::runtime_entry::build_assembly`.
//! - [`configured_principal_ids`] / [`submitter_is_configured`]: schließen die
//!   Restlücke aus `docs/remediation/ledger/W1/W1-13.md` §1 — der Worker kann
//!   prüfen, ob der Einreicher eines Jobs noch ein *aktuell konfigurierter*
//!   MCP-Principal ist.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind synchron und zustandslos; geteilte Speicher werden als
//! `Arc` hereingereicht und nur weitergegeben.
//!
//! # Fehler
//! Fehler erscheinen als `String` (Vertrag von `runtime_entry`); die
//! Sandbox-Fehler sind die `Display`-Form von `harw_runtime::RuntimeError`.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use harw_config::ResolvedConfig;
use harw_core::{ModelProvider, StateStore};
use harw_plan::PlanNodeKind;
use harw_runtime::{
    EntryKind, ModelSource, RuntimeAssembly, RuntimeStores, plan_node_sandbox, root_sandbox,
};
use harw_sandbox::SandboxSpec;
use harw_session_store::JobStore;
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};

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
/// `Channel` × `JobWorker` × `Operator` (CONTRACTS.md §principal). Die Kennung
/// ist die id des authentifizierten Einreichers aus dem gespeicherten Job-Scope,
/// nie ein Wert aus der Job-Eingabe. Auf `JobWorker` liefert
/// [`Principal::approval_actor`] bewusst `None`: ein Job beantwortet keine
/// Freigaben selbst.
///
/// # Arguments
/// - `submitter_id`: Operator-id des Einreichers (`ApprovalActor::Operator { id }`).
#[must_use]
pub(crate) fn job_principal(submitter_id: &str) -> Principal {
    Principal::trusted_ingress(
        PrincipalKind::Channel,
        submitter_id,
        IngressSurface::JobWorker,
        PermissionTier::Operator,
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

/// Montiert die Runtime eines Jobs über `crate::runtime_entry::build_assembly`.
///
/// # Description
/// Speicher: `state_store`, `job_store: Some(..)`, keine durablen Freigaben.
/// Modell: der vom Worker bereits gebaute Provider ([`ModelSource::Override`]).
/// Keine `session_events`: beide Job-Profile haben `SpawnerPolicy::None`
/// (`harw-runtime/src/spec.rs`), die Montage verlangt Events nur für
/// `SpawnerPolicy::BuiltinRoles`.
///
/// # Arguments
/// - `entry`: Job-Art (bestimmt den Einstieg).
/// - `home`, `cwd`: Harness-Home und Arbeitsverzeichnis des Jobs.
/// - `principal`: Principal aus [`job_principal`].
/// - `state_store`, `job_store`: durable Speicher des Workers.
/// - `model`: Provider des Jobs (ggf. budgetiert).
///
/// # Errors
/// Jeder Montagefehler von `build_assembly` als `String`.
pub(crate) fn job_assembly(
    entry: JobEntry,
    home: &Path,
    cwd: &Path,
    principal: Principal,
    state_store: Arc<dyn StateStore>,
    job_store: Arc<JobStore>,
    model: Arc<dyn ModelProvider>,
) -> Result<RuntimeAssembly, String> {
    let spec = crate::runtime_entry::runtime_spec(entry.entry_kind(), home, cwd, principal);
    let stores = RuntimeStores {
        state_store,
        job_store: Some(job_store),
        approval_store: None,
    };
    crate::runtime_entry::build_assembly(spec, ModelSource::Override(model), stores, None)
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

/// Prüft, ob `submitter_id` ein aktuell konfigurierter MCP-Principal ist.
///
/// # Description
/// Exakter Vergleich ohne Normalisierung (kein Trimmen, keine
/// Groß-/Kleinschreibungs-Faltung). Eine leere id ist nie konfiguriert.
/// Schließt die Restlücke aus `ledger/W1/W1-13.md` §1.
#[must_use]
pub(crate) fn submitter_is_configured(config: &ResolvedConfig, submitter_id: &str) -> bool {
    !submitter_id.is_empty()
        && config
            .harness
            .mcp_listener
            .principals
            .iter()
            .any(|principal| principal.id == submitter_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    use harw_config::{HarnessConfig, McpListenerSection, McpPrincipalToml, SecretRef};
    use harw_sandbox::Permission;

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
    fn test_job_principal_is_channel_jobworker_operator() {
        let principal = job_principal("client-7");
        assert_eq!(principal.kind(), PrincipalKind::Channel);
        assert_eq!(principal.id(), "client-7");
        assert_eq!(principal.surface(), IngressSurface::JobWorker);
        assert_eq!(principal.tier(), PermissionTier::Operator);
        assert_eq!(principal.approval_actor(), None);
    }

    #[test]
    fn test_job_sandbox_prompt_has_no_permissions() {
        // tempfile ist dev-dependency von harw-cli; root_sandbox kanonisiert den Pfad.
        let dir = tempfile::tempdir().expect("test: tempdir");
        let sandbox = job_sandbox(JobEntry::Prompt, dir.path()).expect("test: sandbox binds");
        assert_eq!(sandbox.permissions().iter().count(), 0);
        assert!(sandbox.network_scope().is_empty());
    }

    #[test]
    fn test_job_sandbox_research_node_never_writes_or_executes() {
        let dir = tempfile::tempdir().expect("test: tempdir");
        for may_write in [false, true] {
            let entry = JobEntry::PlanNode {
                kind: PlanNodeKind::Research,
                may_write,
            };
            let sandbox = job_sandbox(entry, dir.path()).expect("test: sandbox binds");
            let permissions = sandbox.permissions();
            assert!(permissions.contains(Permission::ReadWorkspace));
            assert!(!permissions.contains(Permission::WriteWorkspace));
            assert!(!permissions.contains(Permission::ExecuteProcess));
            assert!(!permissions.contains(Permission::NetworkAccess));
            assert!(sandbox.network_scope().is_empty());
        }
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
    fn test_submitter_is_configured_rejects_unknown() {
        let config = config_with_principals(&["client-7"]);
        assert!(submitter_is_configured(&config, "client-7"));
        assert!(!submitter_is_configured(&config, "client-8"));
        assert!(!submitter_is_configured(&config, "client-7 "));
        assert!(!submitter_is_configured(&config, "CLIENT-7"));
        assert!(!submitter_is_configured(&config, ""));
        assert!(!submitter_is_configured(&ResolvedConfig::default(), "client-7"));
    }
}
