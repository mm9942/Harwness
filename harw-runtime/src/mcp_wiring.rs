//! MCP-Server als Agent-Werkzeuge der Wurzelsitzung.
//!
//! [`McpContributor`] verbindet beim Bau der Montage jeden in `[mcps.<name>]`
//! mit `enabled = true` konfigurierten Server (Streamable HTTP oder stdio)
//! und hängt einen [`harw_mcp_client::tool_bridge::McpToolProvider`] an die
//! Wurzel-Registry. Die Werkzeuge heißen `mcp.<server>.<tool>`; der Executor
//! prüft selbst `NetworkAccess` + Host (HTTP) bzw. `ExecuteProcess` (stdio).
//!
//! # Laufzeit
//! Die Montage wird außerhalb jedes Tokio-Kontexts gebaut, die TUI startet
//! ihren Runtime erst danach. MCP-Verbindungen (Pipes, HTTP-Pool) brauchen
//! aber einen Reaktor, der die ganze Sitzung lebt: sie entstehen deshalb auf
//! einem eigenen, prozessweiten Runtime ([`mcp_runtime`]) und werden von dort
//! aus getrieben, egal aus welchem Runtime ein Werkzeugaufruf sie pollt.
//!
//! # Fehler
//! Ein nicht erreichbarer oder falsch konfigurierter Server ist nie ein
//! Montagefehler: er fehlt schlicht in der Werkzeugliste und wird per
//! `tracing::warn!` gemeldet.

use std::sync::{Arc, OnceLock};

use harw_config::{McpServerToml, McpTransportToml, ResolvedConfig, SecretRef};
use harw_mcp_client::tool_bridge::{McpEndpoint, McpServerSpec, McpToolProvider};

use crate::contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts};
use crate::error::RuntimeResult;

/// Obergrenze für den gesamten Verbindungsaufbau aller Server.
const CONNECT_BUDGET: std::time::Duration = std::time::Duration::from_secs(45);

/// Prozessweiter Runtime, auf dem alle MCP-Verbindungen leben.
fn mcp_runtime() -> Option<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("harw-mcp")
                .enable_all()
                .build()
                .map_err(|error| tracing::warn!(%error, "mcp.runtime_unavailable"))
                .ok()
        })
        .as_ref()
}

/// Löst eine [`SecretRef`] für ein MCP-Bearer-Token auf. `keyring:`/`secrets:`
/// sind hier (ohne Secret-Store) nicht auflösbar und liefern `None`.
fn resolve_token(reference: &SecretRef) -> Option<Vec<u8>> {
    let value = match reference {
        SecretRef::Env(name) => std::env::var(name).ok()?,
        SecretRef::File(path) => std::fs::read_to_string(path).ok()?,
        SecretRef::FileJson { path, pointer } => {
            let raw = std::fs::read_to_string(path).ok()?;
            let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;
            doc.pointer(pointer)?.as_str()?.to_owned()
        }
        SecretRef::Keyring(_) | SecretRef::Secrets(_) => {
            tracing::warn!(
                reference = %reference.as_ref_string(),
                "mcp.credential_backend_unsupported_here"
            );
            return None;
        }
    };
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.as_bytes().to_vec())
}

/// Übersetzt die konfigurierten, aktiven Server in Verbindungs-Specs.
#[must_use]
pub fn server_specs(config: &ResolvedConfig) -> Vec<McpServerSpec> {
    let mut specs: Vec<McpServerSpec> = config
        .mcps
        .iter()
        .filter(|(_, server)| server.enabled)
        .filter_map(|(key, server)| spec_for(key, server))
        .collect();
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    specs
}

fn spec_for(key: &str, server: &McpServerToml) -> Option<McpServerSpec> {
    let name = if server.name.trim().is_empty() {
        key.to_owned()
    } else {
        server.name.clone()
    };
    let endpoint = match server.transport {
        McpTransportToml::StreamableHttp => {
            let Some(url) = server.url.clone() else {
                tracing::warn!(server = %name, "mcp.http_server_without_url");
                return None;
            };
            McpEndpoint::Http {
                url,
                token: server.auth.as_ref().and_then(resolve_token),
            }
        }
        McpTransportToml::Stdio => {
            let Some(command) = server.command.clone() else {
                tracing::warn!(server = %name, "mcp.stdio_server_without_command");
                return None;
            };
            McpEndpoint::Stdio {
                command,
                args: server.args.clone(),
                env: Vec::new(),
            }
        }
    };
    Some(McpServerSpec {
        name,
        endpoint,
        allowed_tools: server.tools.clone(),
    })
}

/// Verbindet alle Specs auf dem MCP-Runtime (von einem Hilfsthread aus, damit
/// auch ein Aufruf aus einem laufenden Tokio-Kontext nicht blockiert).
fn connect_blocking(specs: Vec<McpServerSpec>) -> Option<Arc<McpToolProvider>> {
    let runtime = mcp_runtime()?;
    let handle = runtime.handle().clone();
    let (provider, warnings) = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                handle.block_on(async move {
                    match tokio::time::timeout(
                        CONNECT_BUDGET,
                        McpToolProvider::connect(specs, "harw", env!("CARGO_PKG_VERSION")),
                    )
                    .await
                    {
                        Ok(result) => Some(result),
                        Err(_) => None,
                    }
                })
            })
            .join()
            .ok()
            .flatten()
    })?;
    for warning in warnings {
        tracing::warn!(%warning, "mcp.server_warning");
    }
    Some(Arc::new(provider))
}

/// Steuert die MCP-Werkzeuge der konfigurierten Server bei.
#[derive(Debug, Default)]
pub struct McpContributor;

impl AssemblyContributor for McpContributor {
    fn contribute(
        &self,
        inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        let specs = server_specs(inputs.config);
        if specs.is_empty() {
            return Ok(());
        }
        let Some(provider) = connect_blocking(specs) else {
            tracing::warn!("mcp.connect_failed_or_timed_out");
            return Ok(());
        };
        let names = provider.tool_names();
        if names.is_empty() {
            return Ok(());
        }
        tracing::info!(tools = names.len(), "mcp.tools_registered");
        let registry = std::mem::take(&mut parts.registry);
        parts.registry = registry.tool_provider(provider);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(transport: McpTransportToml, enabled: bool) -> McpServerToml {
        McpServerToml {
            name: String::new(),
            description: String::new(),
            transport,
            command: Some("mcp-server".to_owned()),
            args: vec!["--stdio".to_owned()],
            url: Some("https://mcp.example.test/mcp".to_owned()),
            auth: Some(SecretRef::Env("HARW_TEST_MCP_TOKEN_UNSET".to_owned())),
            tools: vec!["search".to_owned()],
            enabled,
        }
    }

    #[test]
    fn only_enabled_servers_become_specs_with_key_as_fallback_name() {
        let mut config = ResolvedConfig::default();
        config.mcps.insert(
            "docs".to_owned(),
            server(McpTransportToml::StreamableHttp, true),
        );
        config
            .mcps
            .insert("off".to_owned(), server(McpTransportToml::Stdio, false));
        let specs = server_specs(&config);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "docs");
        assert_eq!(specs[0].allowed_tools, vec!["search".to_owned()]);
        assert!(matches!(
            specs[0].endpoint,
            McpEndpoint::Http { token: None, .. }
        ));
    }

    #[test]
    fn stdio_server_maps_command_and_args() {
        let mut config = ResolvedConfig::default();
        config
            .mcps
            .insert("local".to_owned(), server(McpTransportToml::Stdio, true));
        let specs = server_specs(&config);
        assert!(matches!(
            &specs[0].endpoint,
            McpEndpoint::Stdio { command, args, .. } if command == "mcp-server" && args == &["--stdio".to_owned()]
        ));
    }

    #[test]
    fn keyring_references_are_not_resolved_here() {
        assert!(resolve_token(&SecretRef::Keyring("x".to_owned())).is_none());
    }
}
