//! Typed cloud service control. Device enrollment is a separate identity flow.

use std::sync::Arc;

use harw_home::ResolvedHomeContext;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpFuture, OpOutput};
use serde::Deserialize;

/// Service actions; device enrollment and revocation are deliberately excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

/// Profile-bound service adapter installed by trusted runtime composition.
///
/// The adapter must validate the bound caller/profile against its selected service,
/// preserve cancellation and deadlines, and return bounded diagnostics. An absent
/// adapter never falls back to executing a binary from PATH.
pub trait CloudServiceControl: Send + Sync {
    fn control<'a>(&'a self, ctx: &'a OpContext, action: ServiceAction) -> OpFuture<'a>;
}

/// Arguments of the `cloud.cloudctl.*` operations (none).
#[derive(Debug, Default, Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct CloudctlArgs {}

async fn run_cloudctl(ctx: &OpContext, action: ServiceAction) -> Result<OpOutput, OpError> {
    if ctx.tenant().is_none() || ctx.service::<Arc<ResolvedHomeContext>>().is_none() {
        return Err(OpError::NotAvailable(
            "cloud service control requires a bound caller tenant and home/profile".into(),
        ));
    }
    let control = ctx.service::<Arc<dyn CloudServiceControl>>().ok_or_else(|| {
        OpError::NotAvailable("no profile-bound cloud service control adapter; use explicit local service administration".into())
    })?;
    control.control(ctx, action).await
}

fn device_flow_unavailable() -> Result<OpOutput, OpError> {
    Err(OpError::NotAvailable("device enrollment/revocation requires a device identity workflow; service enable/disable is not enrollment or revocation".into()))
}

/// Start the cloud stack through the bound service adapter.
///
/// # Errors
/// [`OpError::NotAvailable`] when the required identity or service adapter is absent.
#[operation(
    name = "cloud.cloudctl.up",
    summary = "Start the explicitly bound cloud service.",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_up(ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    run_cloudctl(ctx, ServiceAction::Start).await
}

/// Stop the cloud stack through the bound service adapter.
///
/// # Errors
/// [`OpError::NotAvailable`] when the required identity or service adapter is absent.
#[operation(
    name = "cloud.cloudctl.down",
    summary = "Stop the explicitly bound cloud service.",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_down(ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    run_cloudctl(ctx, ServiceAction::Stop).await
}

/// Restart the cloud stack through the bound service adapter.
///
/// # Errors
/// [`OpError::NotAvailable`] when the required identity or service adapter is absent.
#[operation(
    name = "cloud.cloudctl.restart",
    summary = "Restart the explicitly bound cloud service.",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_restart(ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    run_cloudctl(ctx, ServiceAction::Restart).await
}

/// Reserved device enrollment entry point; refuses until identity flow is bound.
///
/// # Errors
/// [`OpError::NotAvailable`] when the required identity or service adapter is absent.
#[operation(
    name = "cloud.cloudctl.enroll",
    summary = "Device enrollment is unavailable until the identity enrollment workflow is bound.",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_enroll(_ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    device_flow_unavailable()
}

/// Reserved device revocation entry point; never disables a service.
///
/// # Errors
/// [`OpError::NotAvailable`] when the required identity or service adapter is absent.
#[operation(
    name = "cloud.cloudctl.revoke",
    summary = "Device revocation is unavailable here; use the canonical device administration flow.",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_revoke(_ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    device_flow_unavailable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_operations::ServiceMap;
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<ServiceAction>>);
    impl CloudServiceControl for Recorder {
        fn control<'a>(&'a self, ctx: &'a OpContext, action: ServiceAction) -> OpFuture<'a> {
            Box::pin(async move {
                assert_eq!(ctx.tenant().map(TenantId::as_str), Some("acme"));
                let home = ctx
                    .service::<Arc<ResolvedHomeContext>>()
                    .expect("bound profile");
                assert_eq!(home.profile_name, "selected");
                self.0.lock().expect("recorder lock").push(action);
                Ok(OpOutput {
                    text: "recorded".into(),
                    data: None,
                })
            })
        }
    }

    fn context(
        root: &std::path::Path,
        adapter: Option<Arc<dyn CloudServiceControl>>,
        tenant: bool,
    ) -> Result<OpContext, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(root.join("ws"))?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("acme"),
                workspace: WorkspaceId::from_str("ws"),
                root: "ws".into(),
            }],
        )?;
        let binding =
            registry.resolve(&TenantId::from_str("acme"), &WorkspaceId::from_str("ws"))?;
        let mut services = ServiceMap::new();
        services.insert(Arc::new(ResolvedHomeContext::new(
            root,
            "selected".into(),
            harw_home::discover_project(root, &[])?,
        )?));
        if let Some(adapter) = adapter {
            services.insert(adapter);
        }
        let ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(binding, PermissionSet::empty()),
            services,
        );
        Ok(if tenant {
            ctx.with_tenant(TenantId::from_str("acme"))
        } else {
            ctx
        })
    }

    #[tokio::test]
    async fn typed_adapter_receives_bound_context_and_only_service_actions()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let recorder = Arc::new(Recorder::default());
        let ctx = context(root.path(), Some(recorder.clone()), true)?;
        cloudctl_up(&ctx, CloudctlArgs {}).await?;
        cloudctl_down(&ctx, CloudctlArgs {}).await?;
        cloudctl_restart(&ctx, CloudctlArgs {}).await?;
        assert!(matches!(
            cloudctl_enroll(&ctx, CloudctlArgs {}).await,
            Err(OpError::NotAvailable(_))
        ));
        assert!(matches!(
            cloudctl_revoke(&ctx, CloudctlArgs {}).await,
            Err(OpError::NotAvailable(_))
        ));
        assert_eq!(
            *recorder.0.lock().expect("recorder lock"),
            [
                ServiceAction::Start,
                ServiceAction::Stop,
                ServiceAction::Restart
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn missing_identity_or_adapter_refuses_before_effects()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let recorder = Arc::new(Recorder::default());
        let untrusted = context(root.path(), Some(recorder.clone()), false)?;
        assert!(matches!(
            cloudctl_up(&untrusted, CloudctlArgs {}).await,
            Err(OpError::NotAvailable(_))
        ));
        assert!(recorder.0.lock().expect("recorder lock").is_empty());
        let unbound = context(root.path(), None, true)?;
        assert!(matches!(
            cloudctl_up(&unbound, CloudctlArgs {}).await,
            Err(OpError::NotAvailable(_))
        ));
        Ok(())
    }
}
