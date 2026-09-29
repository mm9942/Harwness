//! Grammatik von `harw mcp`: Cloudflare-MCP-Integration.

use clap::Subcommand;

use super::values::McpServer;

/// Aktionen für die entfernten MCP-Integrationen (Cloudflare, n8n).
#[derive(Debug, Subcommand)]
pub enum McpAction {
    /// Legt die deklarative MCP-Konfiguration im aktiven Profil an.
    Setup {
        /// MCP-Servername; unterstützt werden `cloudflare` und `n8n`.
        #[arg(value_enum, default_value_t = McpServer::Cloudflare)]
        server: McpServer,
    },
    /// Führt MCP initialize und tools/list gegen den konfigurierten Server aus.
    Check {
        /// MCP-Servername; unterstützt werden `cloudflare` und `n8n`.
        #[arg(value_enum, default_value_t = McpServer::Cloudflare)]
        server: McpServer,
    },
}
