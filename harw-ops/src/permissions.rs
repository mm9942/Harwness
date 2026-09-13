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

use harw_extension_api::ApprovalMode;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

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
        "set" => return set_mode(args.mode.as_deref()),
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

    let active = harw_extension_api::approval_mode::current();
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
/// entscheidet; die Sandbox-Rechte bleiben, wie sie sind.
///
/// # Arguments
/// - `requested` (`Option<&str>`): der gewünschte Modusname.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: kein oder ein unbekannter Name.
fn set_mode(requested: Option<&str>) -> Result<OpOutput, OpError> {
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
    harw_extension_api::approval_mode::set(mode);
    Ok(OpOutput {
        text: format!(
            "Freigabemodus: {} — {}. Gilt ab dem nächsten Werkzeugaufruf.",
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
    use super::{PermissionsArgs, permissions};
    use crate::testutil::toks;
    use harw_extension_api::ApprovalMode;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;

    // Every test that reads or changes the process-wide mode shares this lock.
    // Restoring the default alone does not prevent concurrent assertions racing.
    static MODE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn test_context() -> OpContext {
        let root = std::env::temp_dir().join("harw-permissions-test");
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
        let _guard = MODE_TEST_LOCK.lock().await;
        let ctx = test_context();
        // Der Freigabemodus ist prozessweit; dieser Test setzt ihn nicht und
        // erwartet deshalb die Voreinstellung als aktive Stufe.
        harw_extension_api::approval_mode::set(ApprovalMode::Delegated);
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

    // Der Freigabemodus ist ein prozessweiter Schalter
    // (`harw_extension_api::approval_mode`). Dieser eine Test stellt am Ende
    // ausdrücklich `Delegated` wieder her, damit andere Tests im selben
    // Binary den Startwert vorfinden.
    #[tokio::test]
    async fn permissions_set_full_switches_the_mode_and_reports_it() {
        let _guard = MODE_TEST_LOCK.lock().await;
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
            Ok(output) => {
                assert!(output.text.contains("full"));
                assert_eq!(
                    harw_extension_api::approval_mode::current(),
                    ApprovalMode::FullAccess
                );
            }
            Err(e) => panic!("Unexpected error: {e}"),
        }

        harw_extension_api::approval_mode::set(ApprovalMode::Delegated);
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
        let _guard = MODE_TEST_LOCK.lock().await;
        let ctx = test_context();

        harw_extension_api::approval_mode::set(ApprovalMode::AlwaysAsk);
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

        harw_extension_api::approval_mode::set(ApprovalMode::Delegated);
    }
}
