//! Reusable sandbox permission-guard helpers for [`ToolExecutor`] implementations.
//!
//! # Responsibility
//! Executors must fail closed when their requested authority is not present in
//! the sandbox. This module centralizes the check so every fs / shell / plugin /
//! network tool produces a consistent, structured error message.
//!
//! # Key types
//! - [`require_permission`] — generic single-permission guard.
//! - [`require_host_access`] — network guard combining `Permission::NetworkAccess`
//!   with the sandbox's [`harw_sandbox::NetworkScope`] host allow-list.
//! - [`host_from_url`] — dependency-free URL-to-hostname extraction used to feed
//!   [`require_host_access`].
//!
//! # Concurrency
//! Pure functions; `Send + Sync` at all call sites.
//!
//! # Error types
//! Produces [`crate::ToolOutput::Error`] values; never panics and never returns `Err`.
//!
//! # Examples
//! ```rust,no_run
//! use harw_tools::sandbox_guard::{host_from_url, require_host_access, require_permission};
//! use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
//! use harw_sandbox::Permission;
//!
//! fn dispatch(ctx: &ToolExecutionContext, url: &str) -> Result<ToolOutput, ToolsError> {
//!     if let Some(err) = require_permission(ctx, Permission::ReadWorkspace, "fs.read") {
//!         return Ok(err);
//!     }
//!     if let Some(host) = host_from_url(url) {
//!         if let Some(err) = require_host_access(ctx, &host, "http.fetch") {
//!             return Ok(err);
//!         }
//!     }
//!     Ok(ToolOutput::text("ok"))
//! }
//! ```

use crate::{executor::ToolExecutionContext, output::ToolOutput};
use harw_sandbox::Permission;

/// Returns `Some(ToolOutput::error(...))` when the sandbox does NOT grant the
/// required permission. Callers wrap the result in `Ok(...)` so the check reads
/// as `if let Some(err) = require_permission(...) { return Ok(err); }`.
///
/// # Description
/// Checks whether `ctx.sandbox().permissions().contains(required)`. When the
/// check fails (permission absent) a structured error message of the form
/// `"{tool_name} denied: {required:?} permission missing"` is returned inside a
/// `ToolOutput::Error`. When the check passes, `None` is returned and the caller
/// can proceed with execution.
///
/// # Arguments
/// - `ctx` (`&ToolExecutionContext`): trusted authority established by the harness.
/// - `required` ([`Permission`]): the capability the tool needs.
/// - `tool_name` (`&str`): symbolic tool name used verbatim in the error message.
///
/// # Returns
/// - `None` when the permission is granted — proceed with execution.
/// - `Some(ToolOutput::error(...))` with a message of the form
///   `"{tool_name} denied: {required:?} permission missing"`.
///
/// # Concurrency
/// Pure function; safe to call from any thread.
///
/// # Panics
/// Never.
///
/// # Examples
/// ```rust,no_run
/// use harw_tools::sandbox_guard::require_permission;
/// use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
/// use harw_sandbox::Permission;
///
/// fn dispatch(ctx: &ToolExecutionContext) -> Result<ToolOutput, ToolsError> {
///     if let Some(err) = require_permission(ctx, Permission::ReadWorkspace, "fs.read") {
///         return Ok(err);
///     }
///     Ok(ToolOutput::text("ok"))
/// }
/// ```
#[must_use]
pub fn require_permission(
    ctx: &ToolExecutionContext,
    required: Permission,
    tool_name: &str,
) -> Option<ToolOutput> {
    if ctx.sandbox().permissions().contains(required) {
        None
    } else {
        Some(ToolOutput::error(format!(
            "{tool_name} denied: {required:?} permission missing"
        )))
    }
}

/// Prüft Netzzugriff auf genau einen Host: `Permission::NetworkAccess` UND
/// `sandbox.network_scope().allows(host)` müssen beide erfüllt sein.
///
/// # Description
/// Fail-closed-Kombiprüfung für alle Tools, die eine Netzverbindung zu einem
/// einzelnen Host aufbauen wollen. Fehlt die Permission `NetworkAccess`, wird
/// sofort ein Fehler zurückgegeben, ohne den Netzwerk-Scope zu prüfen. Ist die
/// Permission vorhanden, aber der Host nicht im erlaubten Scope der Sandbox
/// enthalten, wird ebenfalls ein Fehler zurückgegeben. Nur wenn beide Prüfungen
/// erfolgreich sind, liefert die Funktion `None` und der Aufrufer darf
/// fortfahren.
///
/// Die Prüfung bleibt bewusst namensgebunden
/// ([`harw_sandbox::NetworkScope::allows`]), nicht adressgebunden
/// ([`harw_sandbox::NetworkScope::allows_addr`]): dieser Guard läuft als
/// Makro-Prolog *vor* jeder Deserialisierung und jedem Verbindungsaufbau — es
/// gibt an dieser Stelle noch keine aufgelöste Adresse zu prüfen. `allows_addr`
/// würde hier auch nicht weiterhelfen: ein über
/// [`harw_sandbox::NetworkScope::from_hosts`]
/// aus Hostnamen gebauter Scope trägt ausschließlich
/// [`harw_sandbox::EgressTarget::DnsSuffix`]-Ziele, nie
/// [`harw_sandbox::EgressTarget::Cidr`]-Ziele, gegen die `allows_addr`
/// auswerten könnte.
///
/// # Arguments
/// - `ctx` (`&ToolExecutionContext`): trusted authority, die vom Harness aufgebaut wurde.
/// - `host` (`&str`): reiner Hostname ohne Schema, Userinfo oder Port. Der Aufrufer
///   muss Port und Userinfo vorher abtrennen, z. B. mit [`host_from_url`].
/// - `tool_name` (`&str`): symbolischer Tool-Name, wird wörtlich in der Fehlermeldung verwendet.
///
/// # Returns
/// - `None`, wenn `NetworkAccess` gewährt ist UND `host` im Netzwerk-Scope der Sandbox erlaubt ist.
/// - `Some(ToolOutput::error(...))` mit einer deutschsprachigen Fehlermeldung, die den
///   Tool-Namen und (im Host-Fall) den betroffenen Host nennt, aber keine Geheimnisse enthält.
///
/// # Concurrency
/// Pure function; sicher von jedem Thread aufrufbar.
///
/// # Panics
/// Nie.
///
/// # Examples
/// ```rust,no_run
/// use harw_tools::sandbox_guard::require_host_access;
/// use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
///
/// fn dispatch(ctx: &ToolExecutionContext) -> Result<ToolOutput, ToolsError> {
///     if let Some(err) = require_host_access(ctx, "docs.rs", "http.fetch") {
///         return Ok(err);
///     }
///     Ok(ToolOutput::text("ok"))
/// }
/// ```
#[must_use]
pub fn require_host_access(
    ctx: &ToolExecutionContext,
    host: &str,
    tool_name: &str,
) -> Option<ToolOutput> {
    if !ctx.sandbox().permissions().contains(Permission::NetworkAccess) {
        return Some(ToolOutput::error(format!(
            "Tool '{tool_name}' benötigt Permission NetworkAccess"
        )));
    }
    if !ctx.sandbox().network_scope().allows(host) {
        return Some(ToolOutput::error(format!(
            "Tool '{tool_name}': Host '{host}' ist nicht in der erlaubten Host-Liste dieser Sandbox"
        )));
    }
    None
}

/// Extrahiert den Hostnamen aus einer URL ohne externe Dependency.
///
/// # Description
/// Trennt zunächst ein Schema ab (alles bis einschließlich des ersten `"://"`,
/// falls vorhanden). Vom Rest wird alles ab dem ersten `'/'`, `'?'` oder `'#'`
/// verworfen (Pfad, Query, Fragment). Von der verbleibenden Autorität wird die
/// Userinfo vor einem `'@'` verworfen und der Port nach einem `':'` abgetrennt.
/// Der verbleibende Hostname wird kleingeschrieben zurückgegeben.
///
/// # Arguments
/// - `url` (`&str`): beliebige URL, mit oder ohne Schema, mit oder ohne Userinfo/Port.
///
/// # Returns
/// `Some(String)` mit dem kleingeschriebenen, reinen Hostnamen, oder `None`, wenn
/// die Eingabe leer/ungültig ist oder nach der Extraktion kein Host übrig bleibt.
///
/// # Panics
/// Nie.
///
/// # Examples
/// ```rust
/// use harw_tools::sandbox_guard::host_from_url;
///
/// assert_eq!(
///     host_from_url("https://docs.rs/serde/latest/serde/"),
///     Some("docs.rs".to_owned())
/// );
/// assert_eq!(host_from_url(""), None);
/// ```
#[must_use]
pub fn host_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Schema abtrennen: alles bis einschließlich "://" verwerfen, falls vorhanden.
    let after_scheme = match trimmed.find("://") {
        Some(idx) => &trimmed[idx + 3..],
        None => trimmed,
    };

    // Autorität endet am ersten '/', '?' oder '#' (Beginn von Pfad/Query/Fragment).
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];

    // Userinfo vor dem letzten '@' verwerfen (z. B. "user:pw@host").
    let host_and_port = match authority.rfind('@') {
        Some(idx) => &authority[idx + 1..],
        None => authority,
    };

    // Port nach ':' abtrennen.
    let host = match host_and_port.find(':') {
        Some(idx) => &host_and_port[..idx],
        None => host_and_port,
    };

    let host = host.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::ToolExecutionContext;
    use harw_sandbox::{
        NetworkScope, Permission, PermissionSet, SandboxSpec, WorkspaceRegistration,
        WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;

    /// Creates a temporary directory under `std::env::temp_dir()` scoped to this test run,
    /// builds a [`SandboxSpec`] with the given permissions, and returns both so the caller
    /// can keep the directory alive for the duration of the test.
    fn make_sandbox(test_id: &str, permissions: Vec<Permission>) -> (PathBuf, SandboxSpec) {
        let base = std::env::temp_dir()
            .join("harw_sandbox_guard_tests")
            .join(test_id);
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let registry = WorkspaceRegistry::build(
            &base,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .unwrap();
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .unwrap();
        let spec = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        (base, spec)
    }

    /// Wie [`make_sandbox`], aber mit einem eingeschränkten Netzwerk-Scope, der nur die
    /// übergebenen Hosts erlaubt. Baut auf [`make_sandbox`] auf, sodass dessen bestehende
    /// Aufrufer und Tests unverändert bleiben.
    fn make_sandbox_with_hosts(
        test_id: &str,
        permissions: Vec<Permission>,
        allowed_hosts: Vec<&str>,
    ) -> (PathBuf, SandboxSpec) {
        let (base, spec) = make_sandbox(test_id, permissions);
        let scope = NetworkScope::from_hosts(allowed_hosts.into_iter().map(str::to_owned));
        (base, spec.with_network_scope(scope))
    }

    fn make_ctx(spec: SandboxSpec) -> ToolExecutionContext {
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec)
    }

    /// Granted permission → `None` so callers may proceed.
    #[test]
    fn test_require_permission_returns_none_when_granted() {
        let (_base, spec) = make_sandbox("none_when_granted", vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(spec);

        let result = require_permission(&ctx, Permission::ReadWorkspace, "fs.read");
        assert!(
            result.is_none(),
            "should return None when permission is present"
        );
    }

    /// Missing permission → `Some(ToolOutput::Error)`.
    #[test]
    fn test_require_permission_returns_error_when_missing() {
        let (_base, spec) = make_sandbox("error_when_missing", vec![]); // no permissions at all
        let ctx = make_ctx(spec);

        let result = require_permission(&ctx, Permission::ReadWorkspace, "fs.read");
        assert!(
            result.is_some(),
            "should return Some when permission is absent"
        );
        match result.unwrap() {
            ToolOutput::Error { .. } => {}
            other => panic!("expected ToolOutput::Error, got: {other:?}"),
        }
    }

    /// Error message must contain both the tool name and the permission variant name.
    #[test]
    fn test_error_message_includes_tool_name_and_permission_variant() {
        let (_base, spec) = make_sandbox("msg_tool_and_variant", vec![]);
        let ctx = make_ctx(spec);

        let output = require_permission(&ctx, Permission::ExecuteProcess, "shell.exec")
            .expect("should produce Some when permission missing");

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("shell.exec"),
                    "message must contain tool name, got: {message:?}"
                );
                assert!(
                    message.contains("ExecuteProcess"),
                    "message must contain permission variant name, got: {message:?}"
                );
            }
            other => panic!("expected ToolOutput::Error, got: {other:?}"),
        }
    }

    /// `NetworkAccess` gewährt und Host im Scope erlaubt → `None`.
    #[test]
    fn test_require_host_access_returns_none_when_granted_and_host_allowed() {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_none_when_allowed",
            vec![Permission::NetworkAccess],
            vec!["docs.rs"],
        );
        let ctx = make_ctx(spec);

        let result = require_host_access(&ctx, "docs.rs", "http.fetch");
        assert!(
            result.is_none(),
            "should return None when NetworkAccess is granted and host is allowed"
        );
    }

    /// Fehlende `NetworkAccess`-Permission → `Some(error)`, Meldung nennt `NetworkAccess`.
    #[test]
    fn test_require_host_access_returns_error_when_permission_missing() {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_error_when_permission_missing",
            vec![], // keine Permissions vorhanden
            vec!["docs.rs"],
        );
        let ctx = make_ctx(spec);

        let output = require_host_access(&ctx, "docs.rs", "http.fetch")
            .expect("should produce Some when NetworkAccess is missing");

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("NetworkAccess"),
                    "message must name the missing permission, got: {message:?}"
                );
                assert!(
                    message.contains("http.fetch"),
                    "message must contain tool name, got: {message:?}"
                );
            }
            other => panic!("expected ToolOutput::Error, got: {other:?}"),
        }
    }

    /// `NetworkAccess` gewährt, aber Host nicht im Scope → `Some(error)`, Meldung nennt den Host.
    #[test]
    fn test_require_host_access_returns_error_when_host_not_allowed() {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_error_when_host_not_allowed",
            vec![Permission::NetworkAccess],
            vec!["docs.rs"],
        );
        let ctx = make_ctx(spec);

        let output = require_host_access(&ctx, "evil.example", "http.fetch")
            .expect("should produce Some when host is not in the allowed scope");

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("evil.example"),
                    "message must name the disallowed host, got: {message:?}"
                );
                assert!(
                    message.contains("http.fetch"),
                    "message must contain tool name, got: {message:?}"
                );
            }
            other => panic!("expected ToolOutput::Error, got: {other:?}"),
        }
    }

    /// Regression: die Punktgrenzen-Regel aus `NetworkScope::allows` gilt
    /// unverändert auch über diesen Guard — `evildocs.rs` teilt sich die
    /// Endung mit dem erlaubten `docs.rs`, steht aber an keiner Punktgrenze
    /// und bleibt deshalb abgelehnt, während `docs.rs` selbst erlaubt bleibt.
    #[test]
    fn test_require_host_access_rejects_evildocs_despite_shared_suffix() {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_rejects_evildocs_shared_suffix",
            vec![Permission::NetworkAccess],
            vec!["docs.rs"],
        );
        let ctx = make_ctx(spec);

        assert!(
            require_host_access(&ctx, "docs.rs", "http.fetch").is_none(),
            "docs.rs selbst muss weiterhin erlaubt sein"
        );

        let output = require_host_access(&ctx, "evildocs.rs", "http.fetch")
            .expect("evildocs.rs darf nicht durch bloße Endung durchrutschen");
        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("evildocs.rs"),
                    "message must name the disallowed host, got: {message:?}"
                );
            }
            other => panic!("expected ToolOutput::Error, got: {other:?}"),
        }
    }

    /// URL mit Schema, Pfad und ohne Userinfo/Port → reiner Hostname.
    #[test]
    fn test_host_from_url_strips_scheme_and_path() {
        assert_eq!(
            host_from_url("https://docs.rs/serde/latest/serde/"),
            Some("docs.rs".to_owned())
        );
    }

    /// URL mit Userinfo, gemischter Groß-/Kleinschreibung und Port → lowercase Host ohne Port.
    #[test]
    fn test_host_from_url_strips_userinfo_port_and_lowercases() {
        assert_eq!(
            host_from_url("http://user:pw@Example.COM:8443/x"),
            Some("example.com".to_owned())
        );
    }

    /// URL ohne Schema → Host wird trotzdem korrekt erkannt.
    #[test]
    fn test_host_from_url_without_scheme() {
        assert_eq!(host_from_url("docs.rs/x"), Some("docs.rs".to_owned()));
    }

    /// Leere Eingabe → `None`.
    #[test]
    fn test_host_from_url_empty_input_returns_none() {
        assert_eq!(host_from_url(""), None);
    }

    /// Nur ein Schema ohne verbleibenden Host → `None`.
    #[test]
    fn test_host_from_url_scheme_only_returns_none() {
        assert_eq!(host_from_url("https://"), None);
    }
}
