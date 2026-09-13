//! `/permissions` — Sandbox-Rechte ansehen und den Freigabemodus wählen.
//!
//! Die Operation zeigt Workspace-Identität, kanonischen Root und die erteilten
//! Sandbox-Rechte. Diese Rechte sind unveränderlich: sie stammen aus der
//! Sandbox des laufenden Prozesses, und kein Kommando weitet sie aus.
//!
//! Veränderlich ist dagegen der **Freigabemodus**: wie viel harw ohne
//! Rückfrage tun darf. `set` schaltet zwischen den drei Stufen aus
//! [`harw_extension_api::ApprovalMode`] um. Der Modus verschiebt nur, wer
//! entscheidet — er verschiebt nie die Sandbox-Grenze selbst.
//!
//! # Woher der Modus kommt
//! Die Operation besitzt keinen eigenen Zustand. Sie liest und schreibt
//! ausschließlich die [`ApprovalModeCell`], die eine Kompositionswurzel unter
//! ihrem Typ in die [`ServiceMap`](harw_operations::context::ServiceMap) des
//! [`OpContext`] gelegt hat. Diese Zelle gehört zur Sitzung (und ihren
//! Kind-Sitzungen, die denselben Klon teilen) — **nicht** dem Prozess: `set
//! full` wirkt deshalb nur für diese Session, nie für andere Sitzungen oder
//! Job-Worker im selben Prozess. Fehlt die Zelle in der `ServiceMap` (eine
//! Laufzeit hat sie nicht registriert), liefert die Operation
//! [`OpError::NotAvailable`] — nie einen stillen Ersatzwert.

use harw_extension_api::ApprovalMode;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

// ── Konstanten ───────────────────────────────────────────────────────────────

/// Meldung für den Fall, dass keine [`ApprovalModeCell`] registriert ist.
///
/// Das ist kein Fehler der Operation, sondern eine unvollständige
/// Zusammenstellung der Laufzeit: ohne Zelle gibt es keinen Freigabemodus zu
/// lesen oder zu setzen. Die Antwort sagt das, statt einen Modus zu behaupten.
pub(crate) const NO_APPROVAL_MODE_CELL: &str =
    "In dieser Laufzeit ist keine ApprovalModeCell registriert — der Freigabemodus \
     kann weder gelesen noch gewechselt werden. Die Oberfläche muss eine \
     `ApprovalModeCell` in die ServiceMap legen.";

/// Argumente für `/permissions`.
///
/// `show` und das leere Argument zeigen beide die aktuelle Sandbox samt
/// Freigabemodus. `set <ask|auto|full>` wählt den Modus. `revoke` und alles
/// Unbekannte werden abgelehnt: die Sandbox-Rechte selbst sind in diesem
/// Kontext unveränderlich.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct PermissionsArgs {
    /// Unterkommando; `None` gilt als `show`.
    #[serde(default)]
    #[raw(first)]
    pub cmd: Option<String>,
    /// Der Modusname für `set`.
    #[serde(default)]
    #[raw(nth = 1)]
    pub mode: Option<String>,
}

/// Render the current immutable sandbox permission surface.
#[operation(
    name = "permissions",
    summary = "Shows the current immutable sandbox workspace and permissions.",
    domain = "catalog_config",
    // `operator`, nicht `maintainer`: die Operation zeigt die eigene Sandbox und
    // wählt den Freigabemodus der eigenen Sitzung. Beides liegt ohnehin in der
    // Hand der Person am Terminal — sie beantwortet jede Rückfrage selbst. Mit
    // `maintainer` wäre der Befehl in der TUI (`PermissionTier::Operator`)
    // sichtbar, aber nicht ausführbar.
    permission = "operator",
    command(path = "/permissions", visibility = "tui_only"),
    // Web-Fläche: nicht mehr `readonly`, seit `set` den Freigabemodus umschaltet.
    // `approval = "always"`, weil genau dieser Aufruf bestimmt, wie viel ohne
    // Rückfrage geschieht — er darf nicht selbst ohne Rückfrage laufen.
    web(path = "/api/permissions", approval = "always")
)]
async fn permissions(ctx: &OpContext, args: PermissionsArgs) -> Result<OpOutput, OpError> {
    let sub = args.cmd.as_deref().unwrap_or("show");
    match sub {
        "show" => {}
        "set" => return set_mode(ctx, args.mode.as_deref()),
        other => {
            return Err(OpError::NotAvailable(format!(
                "/permissions {other}: nur `show` und `set <{}>` sind verfügbar; die Sandbox-Rechte selbst sind unveränderlich",
                mode_names()
            )));
        }
    }

    let sandbox = ctx.sandbox();
    let workspace = sandbox.workspace();
    let permissions = sandbox
        .permissions()
        .iter()
        .map(|permission| format!("- {permission:?}"))
        .collect::<Vec<_>>();
    let permissions = if permissions.is_empty() {
        "- (none)".to_owned()
    } else {
        permissions.join("\n")
    };

    let Some(cell) = ctx.service::<ApprovalModeCell>() else {
        return Err(OpError::NotAvailable(NO_APPROVAL_MODE_CELL.to_owned()));
    };
    let active = cell.get();
    let modes = ApprovalMode::ALL
        .iter()
        .map(|mode| {
            let marker = if *mode == active { "*" } else { " " };
            format!("{marker} {} — {}", mode.as_str(), mode.description())
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(OpOutput {
        text: format!(
            "Workspace: {}\nTenant: {}\nRoot: {}\nGranted permissions:\n{permissions}\n\nFreigabemodus (* = aktiv):\n{modes}\n\nUmschalten mit `/permissions set <{}>`.",
            workspace.workspace(),
            workspace.tenant(),
            workspace.canonical_root().display(),
            mode_names(),
        ),
    })
}

/// Schaltet den Freigabemodus um.
///
/// # Description
/// Der Modus gilt ab dem nächsten Werkzeugaufruf, auch mitten in einem
/// laufenden Turn. Er verschiebt ausschließlich, wer über einen Aufruf
/// entscheidet; die Sandbox-Rechte bleiben, wie sie sind. Geschrieben wird
/// ausschließlich die [`ApprovalModeCell`] dieser Sitzung — `set full` wirkt
/// damit nur für diese Session, nie prozessweit.
///
/// # Arguments
/// - `ctx` (`&OpContext`): liefert die `ServiceMap` mit der `ApprovalModeCell`.
/// - `requested` (`Option<&str>`): der gewünschte Modusname.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: kein oder ein unbekannter Name.
/// - [`OpError::NotAvailable`]: keine `ApprovalModeCell` registriert.
fn set_mode(ctx: &OpContext, requested: Option<&str>) -> Result<OpOutput, OpError> {
    let Some(requested) = requested.map(str::trim).filter(|name| !name.is_empty()) else {
        return Err(OpError::InvalidArguments(format!(
            "/permissions set braucht einen Modus: {}",
            mode_names()
        )));
    };
    let Some(mode) = ApprovalMode::parse(requested) else {
        return Err(OpError::InvalidArguments(format!(
            "unbekannter Freigabemodus `{requested}`; verfügbar: {}",
            mode_names()
        )));
    };
    let Some(cell) = ctx.service::<ApprovalModeCell>() else {
        return Err(OpError::NotAvailable(NO_APPROVAL_MODE_CELL.to_owned()));
    };
    cell.set(mode);
    Ok(OpOutput {
        text: format!(
            "Freigabemodus: {} — {}. Gilt ab dem nächsten Werkzeugaufruf (nur für diese Sitzung).",
            mode.as_str(),
            mode.description()
        ),
    })
}

/// Die wählbaren Modusnamen als `ask|auto|full`.
fn mode_names() -> String {
    ApprovalMode::ALL
        .iter()
        .map(|mode| mode.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::{ApprovalModeCell, PermissionsArgs, permissions};
    use crate::testutil::toks;
    use harw_extension_api::ApprovalMode;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Baut einen [`OpContext`] mit leerer [`ServiceMap`] — keine
    /// `ApprovalModeCell` registriert. Jeder Test bekommt eine eigene
    /// Workspace-Wurzel (statt eines globalen Zustands), damit Tests parallel
    /// laufen können, ohne sich gegenseitig zu stören.
    fn test_context() -> OpContext {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-permissions-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("resolve workspace binding");
        OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy([Permission::WriteWorkspace, Permission::ReadWorkspace]),
            ),
            ServiceMap::new(),
        )
    }

    /// Wie [`test_context`], aber mit einer eigenen [`ApprovalModeCell`]
    /// (Startwert `mode`) in der `ServiceMap`. Jeder Test, der eine Cell
    /// braucht, bekommt seine eigene — kein geteilter, globaler Zustand.
    fn test_context_with_mode(mode: ApprovalMode) -> (OpContext, ApprovalModeCell) {
        let root = {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!(
                "harw-permissions-test-cell-{}-{id}",
                std::process::id()
            ))
        };
        std::fs::create_dir_all(root.join("workspace")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("resolve workspace binding");
        let cell = ApprovalModeCell::new(mode);
        let mut services = ServiceMap::new();
        services.insert(cell.clone());
        let ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy([Permission::WriteWorkspace, Permission::ReadWorkspace]),
            ),
            services,
        );
        (ctx, cell)
    }

    #[test]
    fn test_permissions_args_from_raw_args_sets_cmd() {
        let args = PermissionsArgs::from_raw_args(&toks(&["show"]));
        match args {
            Ok(a) => assert_eq!(a.cmd.as_deref(), Some("show")),
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn test_permissions_args_from_raw_args_empty_tokens_sets_cmd_none() {
        let args = PermissionsArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.cmd.is_none()),
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn permissions_show_and_default_render_deterministically() {
        let (ctx, _cell) = test_context_with_mode(ApprovalMode::Delegated);
        let expected = format!(
            "Workspace: workspace\nTenant: test-tenant\nRoot: {}\nGranted permissions:\n- ReadWorkspace\n- WriteWorkspace\n\nFreigabemodus (* = aktiv):\n  ask — {}\n* auto — {}\n  full — {}\n\nUmschalten mit `/permissions set <ask|auto|full>`.",
            ctx.sandbox().workspace().canonical_root().display(),
            ApprovalMode::AlwaysAsk.description(),
            ApprovalMode::Delegated.description(),
            ApprovalMode::FullAccess.description(),
        );

        let default_output = permissions(&ctx, PermissionsArgs::default())
            .await
            .expect("show");
        let show_output = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("show".to_owned()),
                mode: None,
            },
        )
        .await
        .expect("show");

        assert_eq!(default_output.text, expected);
        assert_eq!(show_output.text, expected);
    }

    #[tokio::test]
    async fn permissions_show_without_a_cell_is_not_available() {
        let ctx = test_context();

        let result = permissions(&ctx, PermissionsArgs::default()).await;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("ApprovalModeCell"));
            }
            other => panic!("expected NotAvailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permissions_rejects_capability_mutation() {
        let ctx = test_context();
        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("revoke".to_owned()),
                mode: None,
            },
        )
        .await;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("revoke"));
                assert!(message.contains("unveränderlich"));
            }
            other => panic!("expected unavailable mutation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permissions_set_full_switches_only_this_cell_and_reports_it() {
        let (ctx, cell) = test_context_with_mode(ApprovalMode::Delegated);
        let other_cell = ApprovalModeCell::new(ApprovalMode::Delegated);

        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("set".to_owned()),
                mode: Some("full".to_owned()),
            },
        )
        .await;

        match result {
            Ok(output) => {
                assert!(output.text.contains("full"));
                assert_eq!(cell.get(), ApprovalMode::FullAccess);
                // Eine unabhängige Zelle bleibt unberührt — `set` wirkt nur
                // auf die Cell dieser Session, nicht prozessweit.
                assert_eq!(other_cell.get(), ApprovalMode::Delegated);
            }
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn permissions_set_without_a_cell_is_not_available() {
        let ctx = test_context();

        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("set".to_owned()),
                mode: Some("full".to_owned()),
            },
        )
        .await;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("ApprovalModeCell"));
            }
            other => panic!("expected NotAvailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permissions_set_without_mode_returns_invalid_arguments() {
        let ctx = test_context();

        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("set".to_owned()),
                mode: None,
            },
        )
        .await;

        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("/permissions set"));
            }
            other => panic!("expected invalid arguments, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permissions_set_unknown_mode_returns_invalid_arguments() {
        let ctx = test_context();

        let result = permissions(
            &ctx,
            PermissionsArgs {
                cmd: Some("set".to_owned()),
                mode: Some("quatsch".to_owned()),
            },
        )
        .await;

        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("quatsch"));
            }
            other => panic!("expected invalid arguments, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permissions_show_lists_all_modes_and_marks_the_active_one() {
        let (ctx, _cell) = test_context_with_mode(ApprovalMode::AlwaysAsk);

        let output = permissions(&ctx, PermissionsArgs::default())
            .await
            .expect("show");

        for mode in ApprovalMode::ALL {
            assert!(
                output.text.contains(mode.as_str()),
                "expected mode {mode:?} to be listed"
            );
        }
        assert!(output.text.contains("* ask"));
    }
}
