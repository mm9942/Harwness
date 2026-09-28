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

/// Grund, warum eine [`SecretRef`] hier nicht zu einem Token aufgelöst
/// werden konnte (nur für die Warnung in [`spec_for`], kein `harw-config`-Typ).
#[derive(Debug)]
enum TokenResolveError {
    /// Umgebungsvariable fehlt oder ist nicht in gültigem UTF-8 gesetzt.
    EnvUnavailable,
    /// Datei fehlt oder ist nicht lesbar.
    FileUnreadable,
    /// Dateiinhalt ist kein gültiges JSON.
    InvalidJson,
    /// JSON-Pointer verweist auf kein vorhandenes String-Feld.
    PointerMissing,
    /// Aufgelöster Wert ist leer (nur Leerraum).
    Empty,
    /// `keyring:`/`secrets:` sind hier (ohne Secret-Store) nie auflösbar.
    BackendUnsupported,
}

impl TokenResolveError {
    /// Kurzform für `tracing`-Felder (kein `Display`, damit niemand versucht
    /// ist, den Grund als Nutzertext auszugeben statt ihn zu loggen).
    const fn as_str(&self) -> &'static str {
        match self {
            Self::EnvUnavailable => "env_unavailable",
            Self::FileUnreadable => "file_unreadable",
            Self::InvalidJson => "invalid_json",
            Self::PointerMissing => "pointer_missing",
            Self::Empty => "empty",
            Self::BackendUnsupported => "backend_unsupported",
        }
    }
}

/// Löst eine [`SecretRef`] für ein MCP-Bearer-Token auf. `keyring:`/`secrets:`
/// sind hier (ohne Secret-Store) nicht auflösbar und liefern `Err`.
///
/// Ein `Err` heißt: der Aufrufer darf sich **nicht** mit diesem Server
/// verbinden, ohne das konfigurierte Token — [`spec_for`] lässt den Server
/// deshalb ganz aus, statt ihn unauthentifiziert zu verbinden.
fn resolve_token(reference: &SecretRef) -> Result<Vec<u8>, TokenResolveError> {
    let value = match reference {
        SecretRef::Env(name) => {
            std::env::var(name).map_err(|_| TokenResolveError::EnvUnavailable)?
        }
        SecretRef::File(path) => {
            std::fs::read_to_string(path).map_err(|_| TokenResolveError::FileUnreadable)?
        }
        SecretRef::FileJson { path, pointer } => {
            let raw =
                std::fs::read_to_string(path).map_err(|_| TokenResolveError::FileUnreadable)?;
            let doc: serde_json::Value =
                serde_json::from_str(&raw).map_err(|_| TokenResolveError::InvalidJson)?;
            doc.pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .ok_or(TokenResolveError::PointerMissing)?
                .to_owned()
        }
        SecretRef::Keyring(_) | SecretRef::Secrets(_) => {
            tracing::warn!(
                reference = %reference.as_ref_string(),
                "mcp.credential_backend_unsupported_here"
            );
            return Err(TokenResolveError::BackendUnsupported);
        }
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(TokenResolveError::Empty);
    }
    Ok(trimmed.as_bytes().to_vec())
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
            // Ein konfiguriertes Auth, das sich nicht auflösen lässt, darf
            // den Server nie unauthentifiziert verbinden (er würde entweder
            // mit einem unklaren 401 scheitern oder, schlimmer, anonym eine
            // andere Werkzeugmenge zeigen als konfiguriert) — der Server
            // fehlt dann schlicht in der Werkzeugliste, wie jeder andere
            // Fehlkonfigurations-Fall in dieser Funktion.
            let token = match &server.auth {
                Some(reference) => match resolve_token(reference) {
                    Ok(token) => Some(token),
                    Err(error) => {
                        tracing::warn!(
                            server = %name,
                            reference = %reference.as_ref_string(),
                            reason = error.as_str(),
                            "mcp.auth_unresolved_server_skipped"
                        );
                        return None;
                    }
                },
                None => None,
            };
            McpEndpoint::Http { url, token }
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
                    tokio::time::timeout(
                        CONNECT_BUDGET,
                        McpToolProvider::connect(specs, "harw", env!("CARGO_PKG_VERSION")),
                    )
                    .await
                    .ok()
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
    use crate::test_support::{TestError, TestResult};

    /// Baut einen Testserver ohne Auth (die meisten Tests hier prüfen nichts
    /// Auth-Bezogenes); [`http_server_with_unresolvable_auth_is_skipped`]
    /// setzt `auth` selbst.
    fn server(transport: McpTransportToml, enabled: bool) -> McpServerToml {
        McpServerToml {
            name: String::new(),
            description: String::new(),
            transport,
            command: Some("mcp-server".to_owned()),
            args: vec!["--stdio".to_owned()],
            url: Some("https://mcp.example.test/mcp".to_owned()),
            auth: None,
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
        assert!(resolve_token(&SecretRef::Keyring("x".to_owned())).is_err());
    }

    /// Finding: ein HTTP-Server mit konfiguriertem `auth`, dessen Geheimnis
    /// sich nicht auflösen lässt, darf nicht ohne Token verbunden werden —
    /// er fehlt ganz in der Spec-Liste statt anonym zu verbinden.
    #[test]
    fn http_server_with_unresolvable_auth_is_skipped() {
        let mut config = ResolvedConfig::default();
        let mut docs = server(McpTransportToml::StreamableHttp, true);
        docs.auth = Some(SecretRef::Env("HARW_TEST_MCP_TOKEN_UNSET".to_owned()));
        config.mcps.insert("docs".to_owned(), docs);
        let specs = server_specs(&config);
        assert!(
            specs.is_empty(),
            "ein nicht auflösbares Auth darf den Server nicht unauthentifiziert verbinden"
        );
    }

    /// Auflösbares Auth landet als Token in der Spec (nicht `None`, nicht
    /// übersprungen) — die Gegenprobe zu
    /// [`http_server_with_unresolvable_auth_is_skipped`].
    #[test]
    fn http_server_with_resolvable_auth_carries_the_token() -> TestResult {
        let dir = tempfile::tempdir()?;
        let token_path = dir.path().join("token.txt");
        std::fs::write(&token_path, "s3cr3t\n")?;
        let token_path = token_path
            .to_str()
            .ok_or_else(|| TestError::Unexpected("Token-Pfad ist kein UTF-8".to_owned()))?
            .to_owned();
        let mut config = ResolvedConfig::default();
        let mut docs = server(McpTransportToml::StreamableHttp, true);
        docs.auth = Some(SecretRef::File(token_path));
        config.mcps.insert("docs".to_owned(), docs);
        let specs = server_specs(&config);
        let [spec] = specs.as_slice() else {
            return Err(TestError::Unexpected(format!(
                "erwartete genau eine Spec, bekam {}",
                specs.len()
            )));
        };
        match &spec.endpoint {
            McpEndpoint::Http {
                token: Some(token), ..
            } => {
                assert_eq!(token, b"s3cr3t");
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "erwartete ein aufgelöstes Token, bekam {other:?}"
            ))),
        }
    }
}
