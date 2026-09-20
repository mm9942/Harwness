//! CLI-Komposition für entfernte MCP-Server.
//!
//! `harw mcp setup cloudflare` schreibt nur eine Secret-Referenz. Die
//! eigentliche Verbindung wird mit `harw mcp check cloudflare` geprüft.

use std::path::{Path, PathBuf};

use harw_config::{McpServerToml, McpTransportToml};
use harw_mcp_client::McpClient;

use crate::cli::McpAction;
use crate::home::resolve_home;
use crate::mcp_auth::resolve_mcp_credential;

const CLOUDFLARE_ENDPOINT: &str = "https://mcp.cloudflare.com/mcp";
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
        McpAction::Setup { server } => setup(&home, &server),
        McpAction::Check { server } => check(&home, &server),
    }
}

fn setup(home: &Path, server: &str) -> Result<(), String> {
    ensure_cloudflare_name(server)?;
    crate::home::ensure_home(home).map_err(|error| error.to_string())?;
    let profile = harw_home::active_profile_name(home);
    let profile_dir = harw_home::profile_dir(home, &profile).map_err(|error| error.to_string())?;
    let mcp_dir = profile_dir.join("mcps");
    std::fs::create_dir_all(&mcp_dir)
        .map_err(|error| format!("could not create '{}': {error}", mcp_dir.display()))?;
    harden_dir(&mcp_dir)?;
    let path = mcp_config_path(home)?;
    if path.exists() {
        let existing = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read '{}': {error}", path.display()))?;
        if existing == CLOUDFLARE_CONFIG {
            println!(
                "Cloudflare MCP ist bereits eingerichtet: {}",
                path.display()
            );
            return Ok(());
        }
        return Err(format!(
            "MCP-Konfiguration existiert bereits und wird nicht überschrieben: {}",
            path.display()
        ));
    }
    std::fs::write(&path, CLOUDFLARE_CONFIG)
        .map_err(|error| format!("could not write '{}': {error}", path.display()))?;
    println!("Cloudflare MCP eingerichtet: {}", path.display());
    println!("Token-Referenz: env:{CLOUDFLARE_TOKEN_ENV} (kein Token wurde gespeichert)");
    Ok(())
}

fn check(home: &Path, server: &str) -> Result<(), String> {
    ensure_cloudflare_name(server)?;
    let path = mcp_config_path(home)?;
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "Cloudflare MCP ist nicht eingerichtet ({}): {error}; zuerst 'harw mcp setup cloudflare' ausführen",
            path.display()
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
    if endpoint != CLOUDFLARE_ENDPOINT {
        return Err(format!(
            "Cloudflare MCP endpoint must be {CLOUDFLARE_ENDPOINT}"
        ));
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

fn mcp_config_path(home: &Path) -> Result<PathBuf, String> {
    let profile = harw_home::active_profile_name(home);
    let profile_dir = harw_home::profile_dir(home, &profile).map_err(|error| error.to_string())?;
    Ok(profile_dir.join("mcps").join("cloudflare-api.toml"))
}

fn ensure_cloudflare_name(server: &str) -> Result<(), String> {
    if matches!(server, "cloudflare" | "cloudflare-api") {
        Ok(())
    } else {
        Err(format!(
            "unbekannter MCP-Server '{server}'; unterstützt wird 'cloudflare'"
        ))
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
