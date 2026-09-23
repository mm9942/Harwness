//! Grammatik von `harw mcp`: Cloudflare-MCP-Integration.

use clap::Subcommand;

use super::values::McpServer;

/// Aktionen für die Cloudflare-MCP-Integration.
#[derive(Debug, Subcommand)]
pub enum McpAction {
    /// Legt die deklarative Cloudflare-MCP-Konfiguration im aktiven Profil an.
    Setup {
        /// MCP-Servername; derzeit wird `cloudflare` unterstützt.
        #[arg(value_enum, default_value_t = McpServer::Cloudflare)]
        server: McpServer,
    },
    /// Führt MCP initialize und tools/list gegen den konfigurierten Server aus.
    Check {
        /// MCP-Servername; derzeit wird `cloudflare` unterstützt.
        #[arg(value_enum, default_value_t = McpServer::Cloudflare)]
        server: McpServer,
    },
}
