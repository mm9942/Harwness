//! CLI-Komposition für entfernte MCP-Server.
//!
//! `harw mcp setup cloudflare` schreibt nur eine Secret-Referenz. Die
//! eigentliche Verbindung wird mit `harw mcp check cloudflare` geprüft.

use std::path::{Path, PathBuf};

use harw_config::{McpServerToml, McpTransportToml};
use harw_mcp_client::McpClient;

use crate::cli::{McpAction, McpServer};
use crate::home::resolve_home;
use crate::mcp_auth::resolve_mcp_credential;

const CLOUDFLARE_ENDPOINT: &str = "https://mcp.cloudflare.com/mcp";

/// Preset der n8n-Brücke (`harw mcp setup n8n`). Die URL ist ein Platzhalter,
/// den der Nutzer nach dem Setup auf seine eigene HTTPS-n8n-MCP-Instanz setzt;
/// das Token liegt nie in der Datei, sondern nur als Profil-Referenz.
const N8N_CONFIG: &str = r#"name = "n8n"
description = "n8n bridge (outbound-only MCP connector; replace url with your instance)"
transport = "streamable_http"
url = "https://n8n.example.invalid/mcp/n8n"
auth = "file:~/.harw/credentials/n8n/default.token"
tools = []
enabled = false
"#;

/// Standard-Endpoint der n8n-MCP-Brücke; muss vom Nutzer auf die eigene
/// Instanz gesetzt werden, ehe `check` eine Live-Verbindung prüft.
const N8N_PLACEHOLDER_HOST: &str = "n8n.example.invalid";
const CLOUDFLARE_TOKEN_ENV: &str = "CLOUDFLARE_API_TOKEN";

const CLOUDFLARE_CONFIG: &str = r#"name = "cloudflare-api"
description = "Cloudflare API through Cloudflare's managed MCP server."
transport = "streamable_http"
url = "https://mcp.cloudflare.com/mcp"
auth = "env:CLOUDFLARE_API_TOKEN"
tools = ["docs", "search", "execute"]
enabled = true
"#;

/// Routes the MCP setup/check subcommands.
pub fn run(home_override: Option<PathBuf>, action: McpAction) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    match action {
        McpAction::Setup { server } => setup(&home, server.as_str()),
        McpAction::Check { server } => check(&home, server.as_str()),
    }
}

fn setup(home: &Path, server: &str) -> Result<(), String> {
    let server = McpServer::from_arg(server)?;
    ensure_cloudflare_name(server)?;
    crate::home::ensure_home(home).map_err(|error| error.to_string())?;
    let profile = harw_home::active_profile_name(home);
    let profile_dir = harw_home::profile_dir(home, &profile).map_err(|error| error.to_string())?;
    let mcp_dir = profile_dir.join("mcps");
    std::fs::create_dir_all(&mcp_dir)
        .map_err(|error| format!("could not create '{}': {error}", mcp_dir.display()))?;
    harden_dir(&mcp_dir)?;
    let path = mcp_config_path(home, server)?;
    let config_text = match server {
        McpServer::Cloudflare => CLOUDFLARE_CONFIG,
        McpServer::N8n => N8N_CONFIG,
    };
    let display_name = match server {
        McpServer::Cloudflare => "Cloudflare MCP",
        McpServer::N8n => "n8n MCP",
    };
    if path.exists() {
        let existing = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read '{}': {error}", path.display()))?;
        if existing == config_text {
            println!(
                "{display_name} ist bereits eingerichtet: {}",
                path.display()
            );
            return Ok(());
        }
        return Err(format!(
            "MCP-Konfiguration existiert bereits und wird nicht überschrieben: {}",
            path.display()
        ));
    }
    std::fs::write(&path, config_text)
        .map_err(|error| format!("could not write '{}': {error}", path.display()))?;
    println!("{display_name} eingerichtet: {}", path.display());
    match server {
        McpServer::Cloudflare => {
            println!("Token-Referenz: env:{CLOUDFLARE_TOKEN_ENV} (kein Token wurde gespeichert)");
        }
        McpServer::N8n => {
            println!(
                "Token-Ablage: <Profil>/credentials/n8n/default.token (Modus 0600, kein Token wurde gespeichert); URL in der TOML vor Aktivierung auf die eigene Instanz setzen"
            );
        }
    }
    Ok(())
}

fn check(home: &Path, server: &str) -> Result<(), String> {
    let server = McpServer::from_arg(server)?;
    ensure_cloudflare_name(server)?;
    let path = mcp_config_path(home, server)?;
    let display_name = match server {
        McpServer::Cloudflare => "Cloudflare MCP",
        McpServer::N8n => "n8n MCP",
    };
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "{display_name} ist nicht eingerichtet ({}): {error}; zuerst 'harw mcp setup {}' ausführen",
            path.display(),
            server.as_str()
        )
    })?;
    let config: McpServerToml = toml::from_str(&raw)
        .map_err(|error| format!("invalid MCP configuration '{}': {error}", path.display()))?;
    if !config.enabled {
        return Err(format!("MCP server '{}' is disabled", config.name));
    }
    if config.transport != McpTransportToml::StreamableHttp {
        return Err(format!(
            "MCP server '{}' is not configured for streamable_http",
            config.name
        ));
    }
    let endpoint = config
        .url
        .as_deref()
        .ok_or_else(|| format!("MCP server '{}' has no URL", config.name))?;
    match server {
        McpServer::Cloudflare => {
            if endpoint != CLOUDFLARE_ENDPOINT {
                return Err(format!(
                    "Cloudflare MCP endpoint must be {CLOUDFLARE_ENDPOINT}"
                ));
            }
        }
        McpServer::N8n => {
            if endpoint.contains(N8N_PLACEHOLDER_HOST) {
                return Err(format!(
                    "n8n MCP endpoint is still the placeholder ({N8N_PLACEHOLDER_HOST}); set your own instance URL in '{}' first",
                    path.display()
                ));
            }
        }
    }
    let token = config
        .auth
        .as_ref()
        .map(resolve_mcp_credential)
        .transpose()
        .map_err(|error| error.to_string())?;
    let mut client = McpClient::new(endpoint, token).map_err(|error| error.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start MCP check runtime: {error}"))?;
    let tools = runtime.block_on(async {
        client
            .initialize("harwness", env!("CARGO_PKG_VERSION"))
            .await?;
        client.list_all_tools().await
    });
    let tools = tools.map_err(|error| error.to_string())?;
    println!("Cloudflare MCP erreichbar: {endpoint}");
    println!("MCP-Protokoll: {}", client.protocol_version());
    println!("Tools ({}):", tools.len());
    for tool in tools {
        println!("- {}", tool.name);
    }
    Ok(())
}

fn mcp_config_path(home: &Path, server: McpServer) -> Result<PathBuf, String> {
    let profile = harw_home::active_profile_name(home);
    let profile_dir = harw_home::profile_dir(home, &profile).map_err(|error| error.to_string())?;
    let file_name = match server {
        McpServer::Cloudflare => "cloudflare-api.toml",
        McpServer::N8n => "n8n-api.toml",
    };
    Ok(profile_dir.join("mcps").join(file_name))
}

fn ensure_cloudflare_name(server: McpServer) -> Result<(), String> {
    match server {
        McpServer::Cloudflare | McpServer::N8n => Ok(()),
    }
}

impl McpServer {
    /// Parses the CLI server argument into the canonical preset.
    fn from_arg(argument: &str) -> Result<Self, String> {
        match argument {
            "cloudflare" | "cloudflare-api" => Ok(Self::Cloudflare),
            "n8n" | "n8n-workflow" => Ok(Self::N8n),
            _ => Err(format!(
                "unbekannter MCP-Server '{argument}'; unterstützt werden 'cloudflare' und 'n8n'"
            )),
        }
    }
}

#[cfg(unix)]
fn harden_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("could not protect '{}': {error}", path.display()))
}

#[cfg(not(unix))]
fn harden_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}
