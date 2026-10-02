//! `harw attach`: an eine Host-Sitzung über `harw-session-remote` anhängen
//! (W00 W04/W06, Scope S09).
//!
//! Endpoint-Auflösung: `--socket PATH`, sonst `--host ALIAS` (lokaler
//! Alias-Speicher, S07), sonst der Standard-Socket
//! `$XDG_RUNTIME_DIR/harw/session.sock`. Danach `connect_unix` -> `RemotePort`
//! -> `harw_tui::attach::run_attach`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_protocol::SessionPort;
use harw_session_remote::alias::AliasStore;
use harw_session_remote::{ConnectOptions, RemotePort, connect_unix};
use harw_tui::attach::{resolve_session, run_attach};

use crate::cli::AttachArgs;

/// Relativer Pfad des Standard-Sockets unterhalb von `$XDG_RUNTIME_DIR`.
///
/// Platzhalter, bis die Konvention des Session-Daemons (#91) auf dieser Basis
/// liegt; dann dort statt hier ableiten.
const DEFAULT_SOCKET_REL: &str = "harw/session.sock";

/// Wohin verbunden wird.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// Lokaler AF_UNIX-Socket.
    Unix(PathBuf),
    /// Own-Cloud-Knoten (Node-Transport) laut Alias.
    Node { alias: String, endpoint: String },
}

/// Standard-Socket aus dem Laufzeitverzeichnis.
///
/// # Errors
/// Ohne `XDG_RUNTIME_DIR` gibt es keinen sicheren Standardort; der Fehler
/// verweist auf `--socket`.
pub(crate) fn default_socket(xdg_runtime_dir: Option<&str>) -> Result<PathBuf, String> {
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => Ok(Path::new(dir).join(DEFAULT_SOCKET_REL)),
        None => Err(
            "kein Standard-Socket: XDG_RUNTIME_DIR ist nicht gesetzt; `--socket PATH` angeben"
                .to_owned(),
        ),
    }
}

/// Verzeichnis des maschinenlokalen Zustands (`HARW_STATE_DIR`, sonst XDG).
pub(crate) fn state_dir(
    harw_state_dir: Option<&str>,
    xdg_state_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, String> {
    fn set(value: Option<&str>) -> Option<&str> {
        value.filter(|s| !s.is_empty())
    }
    if let Some(dir) = set(harw_state_dir) {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = set(xdg_state_home) {
        return Ok(Path::new(dir).join("harw"));
    }
    if let Some(home) = set(home) {
        return Ok(Path::new(home).join(".local/state/harw"));
    }
    Err("kein Zustandsverzeichnis: HARW_STATE_DIR, XDG_STATE_HOME und HOME fehlen".to_owned())
}

/// Wertet einen Alias-Endpoint (`unix:<pfad>` oder `node:<host:port>`) aus.
pub(crate) fn target_from_endpoint(alias: &str, endpoint: &str) -> Result<Target, String> {
    if let Some(path) = endpoint.strip_prefix("unix:") {
        if path.is_empty() {
            return Err(format!("Alias `{alias}`: leerer unix-Pfad"));
        }
        return Ok(Target::Unix(PathBuf::from(path)));
    }
    if endpoint.starts_with("node:") {
        return Ok(Target::Node {
            alias: alias.to_owned(),
            endpoint: endpoint.to_owned(),
        });
    }
    Err(format!(
        "Alias `{alias}`: unbekannter Endpoint `{endpoint}` (erwartet `unix:` oder `node:`)"
    ))
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn resolve_target(args: &AttachArgs) -> Result<Target, String> {
    if let Some(socket) = &args.socket {
        return Ok(Target::Unix(socket.clone()));
    }
    if let Some(alias) = &args.host {
        let dir = state_dir(
            env("HARW_STATE_DIR").as_deref(),
            env("XDG_STATE_HOME").as_deref(),
            env("HOME").as_deref(),
        )?;
        let record = AliasStore::new(&dir)
            .get(alias)
            .map_err(|e| format!("Host-Alias `{alias}`: {e}"))?
            .ok_or_else(|| format!("Host-Alias `{alias}` ist unbekannt"))?;
        return target_from_endpoint(alias, &record.endpoint);
    }
    default_socket(env("XDG_RUNTIME_DIR").as_deref()).map(Target::Unix)
}

/// Führt `harw attach` aus.
///
/// # Errors
/// Deutscher Fehlertext: Auflösung, Verbindung oder Attach-Fehler.
pub fn run(args: &AttachArgs) -> Result<(), String> {
    let target = resolve_target(args)?;
    let path = match target {
        Target::Unix(path) => path,
        Target::Node { alias, endpoint } => {
            return Err(format!(
                "Alias `{alias}` zeigt auf {endpoint}: Attach über den Node-Transport ist in diesem Schnitt noch nicht verdrahtet"
            ));
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Runtime konnte nicht gestartet werden: {e}"))?;
    runtime.block_on(async move {
        let connection = connect_unix(path.clone(), ConnectOptions::new("harw attach"))
            .await
            .map_err(|e| format!("Verbindung zu {} fehlgeschlagen: {e}", path.display()))?;
        let port: Arc<dyn SessionPort> = Arc::new(RemotePort::new(connection));
        let session = match &args.session {
            Some(query) => Some(
                resolve_session(port.as_ref(), query)
                    .await
                    .map_err(|e| e.to_string())?,
            ),
            None => None,
        };
        run_attach(port, session).await.map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(socket: Option<&str>, host: Option<&str>) -> AttachArgs {
        AttachArgs {
            session: None,
            socket: socket.map(PathBuf::from),
            host: host.map(str::to_owned),
        }
    }

    #[test]
    fn explicit_socket_wins() {
        assert_eq!(
            resolve_target(&args(Some("/tmp/x.sock"), None)),
            Ok(Target::Unix(PathBuf::from("/tmp/x.sock")))
        );
    }

    #[test]
    fn default_socket_needs_runtime_dir() {
        assert_eq!(
            default_socket(Some("/run/user/1")),
            Ok(PathBuf::from("/run/user/1/harw/session.sock"))
        );
        assert!(default_socket(None).is_err_and(|m| m.contains("--socket")));
        assert!(default_socket(Some("")).is_err());
    }

    #[test]
    fn state_dir_precedence() {
        assert_eq!(
            state_dir(Some("/s"), Some("/x"), Some("/h")),
            Ok(PathBuf::from("/s"))
        );
        assert_eq!(
            state_dir(None, Some("/x"), Some("/h")),
            Ok(PathBuf::from("/x/harw"))
        );
        assert_eq!(
            state_dir(None, None, Some("/h")),
            Ok(PathBuf::from("/h/.local/state/harw"))
        );
        assert!(state_dir(None, None, None).is_err());
    }

    #[test]
    fn alias_endpoints() {
        assert_eq!(
            target_from_endpoint("a", "unix:/run/h.sock"),
            Ok(Target::Unix(PathBuf::from("/run/h.sock")))
        );
        assert!(matches!(
            target_from_endpoint("a", "node:10.0.0.1:7000"),
            Ok(Target::Node { .. })
        ));
        assert!(target_from_endpoint("a", "unix:").is_err());
        assert!(target_from_endpoint("a", "ftp://x").is_err());
    }

    #[test]
    fn unreachable_socket_is_an_error_not_a_panic() {
        let result = run(&args(Some("/nonexistent/harw-attach-test.sock"), None));
        assert!(result.is_err());
    }
}
