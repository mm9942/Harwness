//! Gemeinsame Test-Helfer der Wissens-Ops (nur `#[cfg(test)]`).
//!
//! Ersetzt die fünf gleichlautenden `temporary_store`-Kopien in
//! `workbench`, `kanban`, `diary`, `palace` und `dream`.

use std::sync::atomic::{AtomicU64, Ordering};

use harw_knowledge::KnowledgeStore;

use crate::test_support::{TestResult, ctx};

/// Laufende Nummer gegen Namenskollisionen paralleler Tests.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Legt einen frischen, leeren Wissensspeicher unter dem Temp-Verzeichnis an.
///
/// # Argumente
/// - `label` — Kennung im Verzeichnisnamen (`harw-ops-knowledge-<label>-…`).
///
/// # Fehler
/// [`crate::test_support::TestError::Context`], wenn Uhr oder Verzeichnis
/// versagen. Aufräumen (`std::fs::remove_dir_all(store.root())`) bleibt Sache
/// des Tests.
pub(crate) fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(ctx("system clock is after epoch"))?
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "harw-ops-knowledge-{label}-{}-{nonce}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).map_err(ctx("create temporary knowledge root"))?;
    Ok(KnowledgeStore::new(&root))
}

/// Baut einen [`harw_operations::OpContext`] mit `services` (leere Sandbox,
/// frische Session) für Tests der Op-Hüllen (Principal, Hub, Store).
///
/// # Fehler
/// [`crate::test_support::TestError::Context`], wenn die Workspace-Bindung
/// nicht aufgebaut werden kann.
pub(crate) fn op_context(
    services: harw_operations::context::ServiceMap,
) -> TestResult<harw_operations::OpContext> {
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "harw-ops-knowledge-ctx-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("ws")).map_err(ctx("create test workspace"))?;
    let registry = WorkspaceRegistry::build(
        &root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("ws"),
            root: std::path::PathBuf::from("ws"),
        }],
    )
    .map_err(ctx("build workspace registry"))?;
    let binding = registry
        .resolve(
            &TenantId::from_str("test-tenant"),
            &WorkspaceId::from_str("ws"),
        )
        .map_err(ctx("resolve workspace binding"))?;
    let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
    Ok(harw_operations::OpContext::new(
        SessionId::new(),
        TurnId::new(),
        sandbox,
        services,
    ))
}
