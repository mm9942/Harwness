//! Context-bound, tenant-filtered listing of the canonical device registry.

use harw_home::ResolvedHomeContext;
use harw_macros::operation;
use harw_node_listener::DeviceRegistry;
use harw_operations::{OpContext, OpError, OpOutput};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

const MAX_REGISTRY_BYTES: usize = 256 * 1024;
const MAX_ROWS: usize = 128;
const MAX_LABEL_CHARS: usize = 160;

/// Arguments of `cloud.enrollments.list` (none).
#[derive(Debug, Default, Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct CloudEnrollmentsListArgs {}

/// List canonical device records from the runtime-bound profile only.
///
/// Requires a caller tenant or explicit local-profile capability and a bound home; never falls back to
/// environment variables. Invalid rows grant nothing and their raw contents
/// are not exposed. Oversized/unreadable registries are refused in full.
///
/// # Errors
/// [`OpError::NotAvailable`] for missing runtime context, or
/// [`OpError::Execution`] for unreadable or oversized registry data.
#[operation(
    name = "cloud.enrollments.list",
    summary = "List the current tenant's enrolled devices from the selected cloud profile, with bounded canonical registry diagnostics.",
    domain = "execution",
    permission = "maintainer",
    model_tool(readonly, approval = "none")
)]
async fn cloud_enrollments_list(
    ctx: &OpContext,
    _args: CloudEnrollmentsListArgs,
) -> Result<OpOutput, OpError> {
    let tenant = ctx.tenant();
    let local_operator = ctx
        .service::<crate::context::LocalProfileOperator>()
        .is_some();
    if tenant.is_none() && !local_operator {
        return Err(OpError::NotAvailable(
            "cloud listing requires a caller tenant or explicit local-profile operator capability"
                .into(),
        ));
    }
    let visibility = if tenant.is_some() {
        "tenant"
    } else {
        "local_profile_operator"
    };
    let home = ctx.service::<Arc<ResolvedHomeContext>>().ok_or_else(|| {
        OpError::NotAvailable("cloud listing requires a bound home and profile".into())
    })?;
    let registry = DeviceRegistry::new(&home.profile_dir.join("session-host/remote"));
    let scan = registry.scan_bounded(MAX_REGISTRY_BYTES).map_err(|_| {
        OpError::Execution(
            "device registry is unreadable, invalid, or exceeds the read limit".into(),
        )
    })?;
    let mut matching = scan
        .records
        .iter()
        .filter(|record| tenant.is_none_or(|tenant| &record.tenant == tenant));
    let mut entries = Vec::new();
    let mut lines = Vec::new();
    let mut text_bytes = 0usize;
    let mut text_full = false;
    for record in matching.by_ref().take(MAX_ROWS) {
        let label_truncated = record.label.chars().nth(MAX_LABEL_CHARS).is_some();
        let mut label: String = record
            .label
            .chars()
            .take(if label_truncated { MAX_LABEL_CHARS - 1 } else { MAX_LABEL_CHARS })
            .map(|c| if c.is_control() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') { '?' } else { c })
            .collect();
        if label_truncated {
            label.push('…');
        }
        let tier = serde_json::to_value(record.tier)
            .map_err(|_| OpError::Execution("invalid device tier".into()))?;
        let status = if record.revoked { "revoked" } else { "active" };
        let line = format!(
            "{} | {} | tenant={} | {} | {status} | approval={} | {label}",
            record.device.as_str(),
            record.node_id.as_str(),
            record.tenant.as_str(),
            tier.as_str().unwrap_or("unknown"),
            record.approve_optin
        );
        if text_bytes.saturating_add(line.len() + 1) > 64 * 1024 {
            text_full = true;
            break;
        }
        text_bytes += line.len() + 1;
        lines.push(line);
        entries.push(json!({
            "device_id": record.device.as_str(), "node_id": record.node_id.as_str(),
            "tenant": record.tenant.as_str(), "tier": tier, "status": status,
            "label": label, "label_truncated": label_truncated, "approval_opt_in": record.approve_optin,
        }));
    }
    let complete = !text_full && matching.next().is_none();
    let mut text = format!(
        "Cloud enrollments ({visibility}): {}{}",
        entries.len(),
        if complete { "" } else { " (truncated)" }
    );
    if !lines.is_empty() {
        text.push('\n');
        text.push_str(&lines.join("\n"));
    }
    if !scan.rejected.is_empty() {
        text.push_str(&format!(
            "\nRegistry diagnostics: {} rejected rows (contents withheld)",
            scan.rejected.len()
        ));
    }

    Ok(OpOutput {
        text,
        data: Some(
            json!({ "enrollments": entries, "complete": complete, "profile": home.profile_name, "visibility": visibility, "diagnostics": { "rejected_rows": scan.rejected.len() } }),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::ServiceMap;
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn context(
        root: &std::path::Path,
        bind_home: bool,
        tenant: bool,
    ) -> Result<OpContext, Box<dyn std::error::Error>> {
        scoped_context(root, bind_home, tenant, false)
    }

    fn scoped_context(
        root: &std::path::Path,
        bind_home: bool,
        tenant: bool,
        local_operator: bool,
    ) -> Result<OpContext, Box<dyn std::error::Error>> {
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("acme"),
                workspace: WorkspaceId::from_str("ws"),
                root: "workspace".into(),
            }],
        )?;
        let binding =
            registry.resolve(&TenantId::from_str("acme"), &WorkspaceId::from_str("ws"))?;
        let mut services = ServiceMap::new();
        if bind_home {
            let project = harw_home::discover_project(&workspace, &[])?;
            services.insert(Arc::new(harw_home::ResolvedHomeContext::new(
                &root.join("home"),
                "selected".into(),
                project,
            )?));
        }
        if local_operator {
            let principal = harw_types::Principal::trusted_ingress(
                harw_types::PrincipalKind::Human,
                "local-operator",
                harw_types::IngressSurface::Tui,
                harw_types::PermissionTier::Operator,
            );
            services.insert(
                crate::context::LocalProfileOperator::from_principal(&principal)
                    .ok_or("local capability missing")?,
            );
        }
        let ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy([Permission::ReadWorkspace]),
            ),
            services,
        );
        Ok(if tenant {
            ctx.with_tenant(TenantId::from_str("acme"))
        } else {
            ctx
        })
    }

    #[tokio::test]
    async fn bound_profile_uses_canonical_order_optin_and_tenant_filter() -> TestResult {
        let root = tempfile::tempdir()?;
        let remote = root
            .path()
            .join("home/profiles/selected/session-host/remote");
        std::fs::create_dir_all(&remote)?;
        std::fs::write(
            remote.join("node-devices.conf"),
            "node-a|device-a|acme|owner|active|Laptop|approve\nnode-b|device-b|foreign|observer|active|Other\nbroken raw secret\n",
        )?;
        let default = root
            .path()
            .join("home/profiles/default/session-host/remote");
        std::fs::create_dir_all(&default)?;
        std::fs::write(
            default.join("node-devices.conf"),
            "wrong-node|wrong-device|acme|owner|active|Wrong\n",
        )?;
        let output = cloud_enrollments_list(
            &context(root.path(), true, true)?,
            CloudEnrollmentsListArgs {},
        )
        .await?;
        let data = output.data.ok_or("missing structured result")?;
        assert_eq!(
            data["enrollments"]
                .as_array()
                .ok_or("missing entries")?
                .len(),
            1
        );
        assert_eq!(data["enrollments"][0]["node_id"], "node-a");
        assert_eq!(data["enrollments"][0]["device_id"], "device-a");
        assert_eq!(data["enrollments"][0]["approval_opt_in"], true);
        assert_eq!(data["complete"], true);
        assert_eq!(data["diagnostics"]["rejected_rows"], 1);
        assert!(!data.to_string().contains("foreign"));
        assert!(!data.to_string().contains("raw secret"));
        assert!(!data.to_string().contains("wrong-device"));
        Ok(())
    }

    #[tokio::test]
    async fn missing_bound_home_and_tenant_refuse_without_ambient_fallback() -> TestResult {
        let root = tempfile::tempdir()?;
        assert!(matches!(
            cloud_enrollments_list(
                &context(root.path(), false, true)?,
                CloudEnrollmentsListArgs {}
            )
            .await,
            Err(OpError::NotAvailable(_))
        ));
        assert!(matches!(
            cloud_enrollments_list(
                &context(root.path(), true, false)?,
                CloudEnrollmentsListArgs {}
            )
            .await,
            Err(OpError::NotAvailable(_))
        ));
        Ok(())
    }
    #[tokio::test]
    async fn missing_registry_is_empty_but_unreadable_or_oversized_is_an_error() -> TestResult {
        let root = tempfile::tempdir()?;
        let ctx = context(root.path(), true, true)?;
        let empty = cloud_enrollments_list(&ctx, CloudEnrollmentsListArgs {}).await?;
        assert_eq!(empty.data.ok_or("missing data")?["enrollments"], json!([]));
        let remote = root
            .path()
            .join("home/profiles/selected/session-host/remote");
        std::fs::create_dir_all(&remote)?;
        let path = remote.join("node-devices.conf");
        std::fs::create_dir(&path)?;
        assert!(matches!(
            cloud_enrollments_list(&ctx, CloudEnrollmentsListArgs {}).await,
            Err(OpError::Execution(_))
        ));
        std::fs::remove_dir(&path)?;
        let file = std::fs::File::create(&path)?;
        file.set_len((MAX_REGISTRY_BYTES + 1) as u64)?;
        assert!(matches!(
            cloud_enrollments_list(&ctx, CloudEnrollmentsListArgs {}).await,
            Err(OpError::Execution(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn row_and_label_limits_are_reported_without_foreign_or_raw_diagnostics() -> TestResult {
        let root = tempfile::tempdir()?;
        let remote = root
            .path()
            .join("home/profiles/selected/session-host/remote");
        std::fs::create_dir_all(&remote)?;
        let rows: String = (0..=MAX_ROWS)
            .map(|n| {
                format!(
                    "node-{n}|device-{n}|acme|observer|active|{}\n",
                    "x".repeat(MAX_LABEL_CHARS + 10)
                )
            })
            .collect();
        std::fs::write(remote.join("node-devices.conf"), rows)?;
        let output = cloud_enrollments_list(
            &context(root.path(), true, true)?,
            CloudEnrollmentsListArgs {},
        )
        .await?;
        let data = output.data.ok_or("missing data")?;
        assert_eq!(data["complete"], false);
        assert_eq!(
            data["enrollments"]
                .as_array()
                .ok_or("missing entries")?
                .len(),
            MAX_ROWS
        );
        assert_eq!(
            data["enrollments"][0]["label"]
                .as_str()
                .ok_or("missing label")?
                .chars()
                .count(),
            MAX_LABEL_CHARS
        );
        Ok(())
    }
    #[tokio::test]
    async fn registered_status_and_control_refuse_missing_caller_binding() -> TestResult {
        use harw_operations::{OpInput, registry::OperationRegistry};
        let root = tempfile::tempdir()?;
        let mut registry = OperationRegistry::new();
        crate::register_cloud(&mut registry)?;
        for name in [
            "cloud.status",
            "cloud.cloudctl.up",
            "cloud.cloudctl.down",
            "cloud.cloudctl.restart",
        ] {
            let op = registry
                .find_by_name(name)
                .ok_or("missing registered operation")?;
            for (home, tenant) in [(false, true), (true, false)] {
                let ctx = context(root.path(), home, tenant)?;
                assert!(
                    matches!(
                        op.run(&ctx, OpInput::model_tool(json!({}))).await,
                        Err(OpError::NotAvailable(_))
                    ),
                    "{name}"
                );
            }
        }
        let ctx = context(root.path(), true, true)?;
        let status = registry
            .find_by_name("cloud.status")
            .ok_or("missing status")?
            .run(&ctx, OpInput::model_tool(json!({})))
            .await?;
        assert_eq!(
            status.data.ok_or("missing status data")?["profile"],
            "selected"
        );
        for name in [
            "cloud.cloudctl.up",
            "cloud.cloudctl.enroll",
            "cloud.cloudctl.revoke",
        ] {
            assert!(matches!(
                registry
                    .find_by_name(name)
                    .ok_or("missing operation")?
                    .run(&ctx, OpInput::model_tool(json!({})))
                    .await,
                Err(OpError::NotAvailable(_))
            ));
        }
        Ok(())
    }
    #[tokio::test]
    async fn explicit_local_profile_scope_lists_all_tenants_but_tenant_scope_always_narrows()
    -> TestResult {
        let root = tempfile::tempdir()?;
        let remote = root
            .path()
            .join("home/profiles/selected/session-host/remote");
        std::fs::create_dir_all(&remote)?;
        std::fs::write(
            remote.join("node-devices.conf"),
            "node-a|device-a|acme|owner|active|Laptop\nnode-b|device-b|other|observer|active|Other\n",
        )?;
        let output = cloud_enrollments_list(
            &scoped_context(root.path(), true, false, true)?,
            CloudEnrollmentsListArgs {},
        )
        .await?;
        assert!(output.text.contains("local_profile_operator"));
        assert!(output.text.contains("device-b"));
        let data = output.data.ok_or("missing data")?;
        assert_eq!(
            data["enrollments"].as_array().ok_or("missing rows")?.len(),
            2
        );
        assert_eq!(data["visibility"], "local_profile_operator");
        let filtered = cloud_enrollments_list(
            &scoped_context(root.path(), true, true, true)?,
            CloudEnrollmentsListArgs {},
        )
        .await?;
        assert!(!filtered.text.contains("device-b"));
        let data = filtered.data.ok_or("missing filtered data")?;
        assert_eq!(data["visibility"], "tenant");
        assert_eq!(
            data["enrollments"]
                .as_array()
                .ok_or("missing filtered rows")?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn labels_cannot_inject_lines_or_bidirectional_overrides() -> TestResult {
        let root = tempfile::tempdir()?;
        let remote = root
            .path()
            .join("home/profiles/selected/session-host/remote");
        std::fs::create_dir_all(&remote)?;
        std::fs::write(
            remote.join("node-devices.conf"),
            "node-a|device-a|acme|owner|active|\u{202e}Laptop\u{001b}\n",
        )?;
        let output = cloud_enrollments_list(
            &context(root.path(), true, true)?,
            CloudEnrollmentsListArgs {},
        )
        .await?;
        assert!(!output.text.contains('\u{202e}'));
        assert!(!output.text.contains('\u{001b}'));
        Ok(())
    }
}
