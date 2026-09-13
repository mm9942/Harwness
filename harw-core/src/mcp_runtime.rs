//! Trusted stdio-MCP process launch bridge.
//!
//! Catalog resolution verifies which MCP was activated; this module consumes
//! that frozen descriptor together with the effective sandbox. Streamable HTTP
//! is the primary remote path; Bubblewrap is retained for explicitly-local
//! stdio worker servers.

use std::ffi::OsString;
use std::process::Child;

use harw_catalog::McpRuntimeDescriptor;
use harw_config::McpTransportToml;
use harw_sandbox::{BwrapCommandPlan, BwrapLauncher, SandboxSpec};
use url::{Host, Url};

use crate::error::{CoreError, CoreResult};

/// Canonical loopback port for the Harwness Streamable HTTP MCP endpoint.
///
/// Remote descriptors still declare their own HTTPS endpoint. This value is
/// reserved for the local harness endpoint so channel adapters, job workers,
/// and local MCP clients do not drift onto unrelated development ports.
pub const DEFAULT_STREAMABLE_HTTP_MCP_PORT: u16 = 1337;

/// Non-secret connection contract for one remote Streamable HTTP MCP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamableHttpMcpPlan {
    pub name: String,
    pub endpoint: Url,
    pub requires_auth: bool,
    pub tools: Vec<String>,
    pub definition_sha256: String,
}

/// Validates a remote MCP endpoint under the effective network permission.
/// Secrets are intentionally absent: a transport client resolves the declared
/// secret reference only at connection time, after this plan was accepted.
pub fn plan_streamable_http_mcp(
    sandbox: &SandboxSpec,
    descriptor: &McpRuntimeDescriptor,
) -> CoreResult<StreamableHttpMcpPlan> {
    if descriptor.transport != McpTransportToml::StreamableHttp {
        return Err(CoreError::McpLaunchRejected {
            name: descriptor.name.clone(),
            reason: "descriptor is not a Streamable HTTP MCP".to_owned(),
        });
    }
    if !sandbox
        .permissions()
        .contains(harw_sandbox::Permission::NetworkAccess)
    {
        return Err(CoreError::McpLaunchRejected {
            name: descriptor.name.clone(),
            reason: "effective sandbox does not permit network access".to_owned(),
        });
    }
    let endpoint = descriptor
        .url
        .as_deref()
        .ok_or_else(|| CoreError::McpLaunchRejected {
            name: descriptor.name.clone(),
            reason: "Streamable HTTP descriptor has no endpoint URL".to_owned(),
        })
        .and_then(|url| {
            Url::parse(url).map_err(|error| CoreError::McpLaunchRejected {
                name: descriptor.name.clone(),
                reason: format!("invalid Streamable HTTP endpoint: {error}"),
            })
        })?;
    let loopback = matches!(endpoint.host(),
        Some(Host::Domain(host)) if host.eq_ignore_ascii_case("localhost")
    ) || matches!(endpoint.host(),
        Some(Host::Ipv4(address)) if address.is_loopback()
    ) || matches!(endpoint.host(),
        Some(Host::Ipv6(address)) if address.is_loopback()
    );
    if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && loopback) {
        return Err(CoreError::McpLaunchRejected {
            name: descriptor.name.clone(),
            reason: "remote Streamable HTTP endpoints must use HTTPS; plain HTTP is loopback-only"
                .to_owned(),
        });
    }
    Ok(StreamableHttpMcpPlan {
        name: descriptor.name.clone(),
        endpoint,
        requires_auth: descriptor.requires_auth,
        tools: descriptor.tools.clone(),
        definition_sha256: descriptor.definition_sha256.clone(),
    })
}

pub fn plan_stdio_mcp(
    launcher: &BwrapLauncher,
    sandbox: &SandboxSpec,
    descriptor: &McpRuntimeDescriptor,
) -> CoreResult<BwrapCommandPlan> {
    if descriptor.transport != McpTransportToml::Stdio {
        return Err(CoreError::McpLaunchRejected {
            name: descriptor.name.clone(),
            reason: "only stdio MCP descriptors can be process-launched".to_owned(),
        });
    }
    let command = descriptor
        .command
        .as_ref()
        .filter(|command| !command.is_empty())
        .ok_or_else(|| CoreError::McpLaunchRejected {
            name: descriptor.name.clone(),
            reason: "stdio descriptor has no executable command".to_owned(),
        })?;
    let mut argv = Vec::with_capacity(descriptor.args.len() + 1);
    argv.push(OsString::from(command));
    argv.extend(descriptor.args.iter().map(OsString::from));
    launcher
        .plan(sandbox, &argv)
        .map_err(|error| CoreError::McpLaunchRejected {
            name: descriptor.name.clone(),
            reason: error.to_string(),
        })
}

/// Starts a verified stdio MCP in its effective OS sandbox. Callers own the
/// returned process handle and must connect it to child cancellation/lease
/// cleanup before exposing the server's tools to a model.
pub fn spawn_stdio_mcp(
    launcher: &BwrapLauncher,
    sandbox: &SandboxSpec,
    descriptor: &McpRuntimeDescriptor,
) -> CoreResult<Child> {
    let plan = plan_stdio_mcp(launcher, sandbox, descriptor)?;
    launcher
        .spawn(&plan)
        .map_err(|error| CoreError::McpLaunchRejected {
            name: descriptor.name.clone(),
            reason: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_sandbox::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{TenantId, WorkspaceId};
    use std::path::PathBuf;

    fn sandbox(permissions: PermissionSet) -> SandboxSpec {
        let root = std::env::temp_dir().join(format!("harwness-mcp-plan-{}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .unwrap();
        SandboxSpec::from_resolved(
            registry
                .resolve(
                    &TenantId::from_str("tenant"),
                    &WorkspaceId::from_str("workspace"),
                )
                .unwrap(),
            permissions,
        )
    }

    fn stdio_descriptor() -> McpRuntimeDescriptor {
        McpRuntimeDescriptor {
            name: "search".to_owned(),
            description: String::new(),
            transport: McpTransportToml::Stdio,
            command: Some("/bin/true".to_owned()),
            args: vec!["--stdio".to_owned()],
            url: None,
            requires_auth: false,
            tools: Vec::new(),
            definition_sha256: "a".repeat(64),
        }
    }

    #[test]
    fn stdio_plan_is_built_only_under_effective_execute_permission() {
        let launcher = BwrapLauncher::default();
        let descriptor = stdio_descriptor();
        let plan = plan_stdio_mcp(
            &launcher,
            &sandbox(PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::ExecuteProcess,
            ])),
            &descriptor,
        )
        .unwrap();
        assert!(
            plan.args()
                .iter()
                .any(|argument| argument == &OsString::from("/bin/true"))
        );

        assert!(matches!(
            plan_stdio_mcp(
                &launcher,
                &sandbox(PermissionSet::from_policy([Permission::ReadWorkspace])),
                &descriptor,
            ),
            Err(CoreError::McpLaunchRejected { .. })
        ));
    }

    #[test]
    fn http_descriptors_cannot_be_misused_as_process_commands() {
        let mut descriptor = stdio_descriptor();
        descriptor.transport = McpTransportToml::StreamableHttp;
        descriptor.url = Some("https://example.invalid/mcp".to_owned());
        assert!(matches!(
            plan_stdio_mcp(
                &BwrapLauncher::default(),
                &sandbox(PermissionSet::from_policy([Permission::ExecuteProcess])),
                &descriptor,
            ),
            Err(CoreError::McpLaunchRejected { .. })
        ));
    }

    #[test]
    fn streamable_http_requires_network_and_secure_remote_endpoint() {
        let mut descriptor = stdio_descriptor();
        descriptor.transport = McpTransportToml::StreamableHttp;
        descriptor.command = None;
        descriptor.url = Some("https://mcp.example.test/mcp".to_owned());
        let plan = plan_streamable_http_mcp(
            &sandbox(PermissionSet::from_policy([Permission::NetworkAccess])),
            &descriptor,
        )
        .unwrap();
        assert_eq!(plan.endpoint.as_str(), "https://mcp.example.test/mcp");

        assert!(matches!(
            plan_streamable_http_mcp(
                &sandbox(PermissionSet::from_policy([Permission::ReadWorkspace])),
                &descriptor,
            ),
            Err(CoreError::McpLaunchRejected { .. })
        ));
        descriptor.url = Some("http://mcp.example.test/mcp".to_owned());
        assert!(matches!(
            plan_streamable_http_mcp(
                &sandbox(PermissionSet::from_policy([Permission::NetworkAccess])),
                &descriptor,
            ),
            Err(CoreError::McpLaunchRejected { .. })
        ));

        descriptor.url = Some(format!(
            "http://[::1]:{DEFAULT_STREAMABLE_HTTP_MCP_PORT}/mcp"
        ));
        let loopback_plan = plan_streamable_http_mcp(
            &sandbox(PermissionSet::from_policy([Permission::NetworkAccess])),
            &descriptor,
        )
        .unwrap();
        assert!(matches!(
            loopback_plan.endpoint.host(),
            Some(Host::Ipv6(address)) if address.is_loopback()
        ));
    }
}
