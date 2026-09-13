use serde::{Deserialize, Serialize};

use crate::auth_toml::SecretRef;

/// Declarative MCP server metadata. It intentionally contains no process
/// execution logic; the runtime must validate and launch a selected server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerToml {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub transport: McpTransportToml,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub auth: Option<SecretRef>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportToml {
    #[default]
    Stdio,
    /// MCP's remote Streamable HTTP transport. `http` remains a parse-only
    /// compatibility alias for early Harwness config drafts; runtime and
    /// generated config always use `streamable_http` explicitly.
    #[serde(rename = "streamable_http", alias = "http")]
    StreamableHttp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_disabled_http_server_metadata() {
        let server: McpServerToml = toml::from_str(
            "name = \"docs\"\ntransport = \"http\"\nurl = \"https://example.invalid/mcp\"\n",
        )
        .unwrap();

        assert_eq!(server.transport, McpTransportToml::StreamableHttp);
        assert!(!server.enabled);
    }

    #[test]
    fn serializes_the_explicit_streamable_http_transport_name() {
        let server: McpServerToml = toml::from_str(
            "name = \"workflow\"\ntransport = \"streamable_http\"\nurl = \"https://mcp.example.test\"\nenabled = true\n",
        )
        .unwrap();
        assert_eq!(server.transport, McpTransportToml::StreamableHttp);
        let encoded = toml::to_string(&server).unwrap();
        assert!(encoded.contains("transport = \"streamable_http\""));
    }

    #[test]
    fn rejects_misspelled_auth_listener_and_transport_fields() {
        for field in ["aut", "listenr", "transprot"] {
            let input = format!("name = \"docs\"\n{field} = \"ignored\"\n");
            assert!(
                toml::from_str::<McpServerToml>(&input).is_err(),
                "misspelled MCP field {field:?} must be rejected"
            );
        }
    }
}
