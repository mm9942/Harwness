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
//!   with the sandbox's [`harw_authority::NetworkScope`] host allow-list.
//! - [`host_from_url`] — URL-to-hostname extraction used to feed
//!   [`require_host_access`]; delegates to [`harw_sandbox::EgressUrl::parse`]
//!   (WHATWG parsing, the same parser reqwest uses to connect).
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
//! use harw_authority::Permission;
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
use harw_authority::Permission;
use harw_sandbox::EgressUrl;

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
/// use harw_authority::Permission;
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
    } else if required == Permission::NetworkAccess {
        Some(ToolOutput::error(format!(
            "{tool_name} denied: {required:?} permission missing — {NETWORK_ENABLE_HINT}"
        )))
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
/// ([`harw_authority::NetworkScope::allows`]), nicht adressgebunden
/// ([`harw_authority::NetworkScope::allows_addr`]): dieser Guard läuft als
/// Makro-Prolog *vor* jeder Deserialisierung und jedem Verbindungsaufbau — es
/// gibt an dieser Stelle noch keine aufgelöste Adresse zu prüfen. `allows_addr`
/// würde hier auch nicht weiterhelfen: ein über
/// [`harw_authority::NetworkScope::from_hosts`]
/// aus Hostnamen gebauter Scope trägt ausschließlich
/// [`harw_authority::EgressTarget::DnsSuffix`]-Ziele, nie
/// [`harw_authority::EgressTarget::Cidr`]-Ziele, gegen die `allows_addr`
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
    if !ctx
        .sandbox()
        .permissions()
        .contains(Permission::NetworkAccess)
    {
        return Some(ToolOutput::error(format!(
            "Tool '{tool_name}' benötigt Permission NetworkAccess — {NETWORK_ENABLE_HINT}"
        )));
    }
    if !ctx.sandbox().network_scope().allows(host) {
        return Some(ToolOutput::error(format!(
            "Tool '{tool_name}': Host '{host}' ist nicht in der erlaubten Host-Liste dieser Sandbox — {NETWORK_ENABLE_HINT}"
        )));
    }
    None
}

/// Hinweis an Modell und Nutzerin, wie sich Netz für die Recherche
/// freischalten lässt (Teil jeder Netz-Ablehnung dieses Guards und der
/// Egress-Ablehnung von `web.fetch`). Ohne Konfiguration hat harw kein Netz.
pub const NETWORK_ENABLE_HINT: &str = "Netz für Recherche freischalten: in der Konfiguration [network].research_web = \"open\" setzen (öffentliches Web, Freigabe je Domain) oder die Hosts in [network].researcher_web_hosts bzw. [network].allow_hosts eintragen";

/// Extrahiert den normalisierten Hostnamen aus einer absoluten `http`/`https`-URL.
///
/// # Description
/// Delegiert vollständig an [`harw_sandbox::EgressUrl::parse`], also an den
/// WHATWG-Parser des `url`-Crates — denselben, den `reqwest` beim
/// Verbindungsaufbau verwendet. Geprüfter und tatsächlich kontaktierter Host
/// können dadurch nicht auseinanderlaufen: `https://evil.com\@docs.rs/` liefert
/// `evil.com`, weil `\` bei Spezial-Schemata die Autorität beendet (Befund
/// F-002). Domains kommen als IDNA-Punycode, kleingeschrieben und ohne
/// abschließenden Punkt zurück; IP-Literale in `std`-Darstellung, IPv6 ohne
/// eckige Klammern (`https://[::1]:8080/` → `::1`, Befund F-167).
///
/// Die frühere, eigene String-Zerlegung ist ersatzlos entfallen. Bewusst
/// strenger als vorher (fail closed): URLs ohne Schema, mit anderem Schema als
/// `http`/`https` oder mit Userinfo (`user:pw@host`) liefern `None`.
///
/// # Arguments
/// - `url` (`&str`): absolute URL.
///
/// # Returns
/// `Some(String)` mit dem normalisierten Host, oder `None`, wenn
/// [`harw_sandbox::EgressUrl::parse`] die Eingabe ablehnt.
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
/// assert_eq!(host_from_url("https://evil.com\\@docs.rs/"), Some("evil.com".to_owned()));
/// assert_eq!(host_from_url(""), None);
/// ```
#[must_use]
pub fn host_from_url(url: &str) -> Option<String> {
    EgressUrl::parse(url).ok().map(|parsed| parsed.host_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::ToolExecutionContext;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        NetworkScope, Permission, PermissionSet, SandboxSpec, WorkspaceRegistration,
        WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;

    /// Creates a temporary directory under `std::env::temp_dir()` scoped to this test run,
    /// builds a [`SandboxSpec`] with the given permissions, and returns both so the caller
    /// can keep the directory alive for the duration of the test.
    fn make_sandbox(
        test_id: &str,
        permissions: Vec<Permission>,
    ) -> TestResult<(PathBuf, SandboxSpec)> {
        let base = std::env::temp_dir()
            .join("harw_sandbox_guard_tests")
            .join(test_id);
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).map_err(ctx("Sandbox-Verzeichnis anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &base,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry bauen"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("Workspace auflösen"))?;
        let spec = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        Ok((base, spec))
    }

    /// Wie [`make_sandbox`], setzt zusätzlich einen echten, eingeschränkten
    /// Netzwerk-Scope, der nur die übergebenen Hosts erlaubt. Nutzt
    /// `SandboxSpec::from_resolved_for_test` (Feature `test-support` von
    /// `harw-authority`, nur in `[dev-dependencies]` dieser Crate) — die
    /// produktive API erlaubt außerhalb von `harw-authority` sonst nur
    /// `NetworkScope::empty()` (`SandboxSpec::from_resolved`).
    fn make_sandbox_with_hosts(
        test_id: &str,
        permissions: Vec<Permission>,
        allowed_hosts: Vec<&str>,
    ) -> TestResult<(PathBuf, SandboxSpec)> {
        let base = std::env::temp_dir()
            .join("harw_sandbox_guard_tests")
            .join(test_id);
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).map_err(ctx("Sandbox-Verzeichnis anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &base,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry bauen"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("Workspace auflösen"))?;
        let spec = SandboxSpec::from_resolved_for_test(
            binding,
            PermissionSet::from_policy(permissions),
            NetworkScope::from_hosts(allowed_hosts.into_iter().map(str::to_owned)),
        );
        Ok((base, spec))
    }

    fn make_ctx(spec: SandboxSpec) -> ToolExecutionContext {
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec)
    }

    /// Granted permission → `None` so callers may proceed.
    #[test]
    fn test_require_permission_returns_none_when_granted() -> TestResult {
        let (_base, spec) = make_sandbox("none_when_granted", vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(spec);

        let result = require_permission(&ctx, Permission::ReadWorkspace, "fs.read");
        assert!(
            result.is_none(),
            "should return None when permission is present"
        );
        Ok(())
    }

    /// Missing permission → `Some(ToolOutput::Error)`.
    #[test]
    fn test_require_permission_returns_error_when_missing() -> TestResult {
        let (_base, spec) = make_sandbox("error_when_missing", vec![])?; // no permissions at all
        let ctx = make_ctx(spec);

        let result = require_permission(&ctx, Permission::ReadWorkspace, "fs.read");
        assert!(
            result.is_some(),
            "should return Some when permission is absent"
        );
        match result.ok_or(TestError::Missing("require_permission Ergebnis"))? {
            ToolOutput::Error { .. } => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected ToolOutput::Error, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Error message must contain both the tool name and the permission variant name.
    #[test]
    fn test_error_message_includes_tool_name_and_permission_variant() -> TestResult {
        let (_base, spec) = make_sandbox("msg_tool_and_variant", vec![])?;
        let ctx = make_ctx(spec);

        let output = require_permission(&ctx, Permission::ExecuteProcess, "shell.exec").ok_or(
            TestError::Missing("should produce Some when permission missing"),
        )?;

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected ToolOutput::Error, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Plan R9: eine Netz-Ablehnung sagt, wie man Netz für die Recherche
    /// freischaltet (`[network].research_web = "open"` oder Hosts eintragen).
    #[test]
    fn test_network_denials_explain_how_to_enable_research_network() -> TestResult {
        let (_base, offline) = make_sandbox("network_hint_offline", vec![])?;
        let ctx = make_ctx(offline);
        for output in [
            require_permission(&ctx, Permission::NetworkAccess, "web.fetch"),
            require_host_access(&ctx, "docs.rs", "web.fetch"),
        ] {
            match output.ok_or(TestError::Missing("Netz-Ablehnung"))? {
                ToolOutput::Error { message } => {
                    assert!(message.contains("research_web = \"open\""), "{message}");
                    assert!(message.contains("researcher_web_hosts"), "{message}");
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected ToolOutput::Error, got: {other:?}"
                    )));
                }
            }
        }
        // Andere Rechte bekommen keinen Netz-Hinweis.
        match require_permission(&ctx, Permission::ExecuteProcess, "shell.exec")
            .ok_or(TestError::Missing("Exec-Ablehnung"))?
        {
            ToolOutput::Error { message } => assert!(!message.contains("research_web")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected ToolOutput::Error, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// `NetworkAccess` gewährt und Host im Scope erlaubt → `None`.
    #[test]
    fn test_require_host_access_returns_none_when_granted_and_host_allowed() -> TestResult {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_none_when_allowed",
            vec![Permission::NetworkAccess],
            vec!["docs.rs"],
        )?;
        let ctx = make_ctx(spec);

        let result = require_host_access(&ctx, "docs.rs", "http.fetch");
        assert!(
            result.is_none(),
            "should return None when NetworkAccess is granted and host is allowed"
        );
        Ok(())
    }

    /// Fehlende `NetworkAccess`-Permission → `Some(error)`, Meldung nennt `NetworkAccess`.
    #[test]
    fn test_require_host_access_returns_error_when_permission_missing() -> TestResult {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_error_when_permission_missing",
            vec![], // keine Permissions vorhanden
            vec!["docs.rs"],
        )?;
        let ctx = make_ctx(spec);

        let output = require_host_access(&ctx, "docs.rs", "http.fetch").ok_or(
            TestError::Missing("should produce Some when NetworkAccess is missing"),
        )?;

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected ToolOutput::Error, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// `NetworkAccess` gewährt, aber Host nicht im Scope → `Some(error)`, Meldung nennt den Host.
    /// Prüft gezielt gegen die Allow-Liste, nicht nur gegen einen leeren Scope:
    /// `docs.rs` selbst bleibt erlaubt, nur `evil.example` wird abgelehnt.
    #[test]
    fn test_require_host_access_returns_error_when_host_not_allowed() -> TestResult {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_error_when_host_not_allowed",
            vec![Permission::NetworkAccess],
            vec!["docs.rs"],
        )?;
        let ctx = make_ctx(spec);

        assert!(
            require_host_access(&ctx, "docs.rs", "http.fetch").is_none(),
            "docs.rs is on the allow list and must remain allowed"
        );

        let output = require_host_access(&ctx, "evil.example", "http.fetch").ok_or(
            TestError::Missing("should produce Some when host is not in the allowed scope"),
        )?;

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected ToolOutput::Error, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Regression: die Punktgrenzen-Regel aus `NetworkScope::allows` gilt
    /// unverändert auch über diesen Guard — `evildocs.rs` teilt sich die
    /// Endung mit dem erlaubten `docs.rs`, steht aber an keiner Punktgrenze
    /// und bleibt deshalb abgelehnt, während `docs.rs` selbst erlaubt bleibt.
    #[test]
    fn test_require_host_access_rejects_evildocs_despite_shared_suffix() -> TestResult {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_rejects_evildocs_shared_suffix",
            vec![Permission::NetworkAccess],
            vec!["docs.rs"],
        )?;
        let ctx = make_ctx(spec);

        assert!(
            require_host_access(&ctx, "docs.rs", "http.fetch").is_none(),
            "docs.rs selbst muss weiterhin erlaubt sein"
        );

        let output = require_host_access(&ctx, "evildocs.rs", "http.fetch").ok_or(
            TestError::Missing("evildocs.rs darf nicht durch bloße Endung durchrutschen"),
        )?;
        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("evildocs.rs"),
                    "message must name the disallowed host, got: {message:?}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected ToolOutput::Error, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// URL mit Schema, Pfad und ohne Userinfo/Port → reiner Hostname.
    #[test]
    fn test_host_from_url_strips_scheme_and_path() {
        assert_eq!(
            host_from_url("https://docs.rs/serde/latest/serde/"),
            Some("docs.rs".to_owned())
        );
    }

    /// Gemischte Groß-/Kleinschreibung und Port → lowercase Host ohne Port.
    #[test]
    fn test_host_from_url_strips_port_and_lowercases() {
        assert_eq!(
            host_from_url("http://Example.COM:8443/x"),
            Some("example.com".to_owned())
        );
    }

    /// Userinfo wird nicht mehr abgeschnitten, sondern abgelehnt (fail closed).
    #[test]
    fn test_host_from_url_rejects_userinfo() {
        assert_eq!(host_from_url("http://user:pw@Example.COM:8443/x"), None);
        assert_eq!(host_from_url("https://docs.rs@evil.com/"), None);
    }

    /// URL ohne Schema → `None`: reqwest könnte sie ohnehin nicht senden.
    #[test]
    fn test_host_from_url_without_scheme_returns_none() {
        assert_eq!(host_from_url("docs.rs/x"), None);
    }

    /// Andere Schemata als http/https → `None`.
    #[test]
    fn test_host_from_url_rejects_non_http_scheme() {
        assert_eq!(host_from_url("ftp://docs.rs/"), None);
        assert_eq!(host_from_url("file:///etc/passwd"), None);
    }

    /// F-002: `\` beendet bei Spezial-Schemata die Autorität — geprüft wird der
    /// Host, zu dem `url`/reqwest tatsächlich verbinden.
    #[test]
    fn test_host_from_url_backslash_bypass_yields_real_host() {
        assert_eq!(
            host_from_url("https://evil.com\\@docs.rs/"),
            Some("evil.com".to_owned())
        );
        assert_eq!(
            host_from_url("https://evil.com\\.docs.rs/"),
            Some("evil.com".to_owned())
        );
    }

    /// F-167: IPv6-Literale liefern die Adresse ohne Klammern und Port.
    #[test]
    fn test_host_from_url_ipv6_literal() {
        let host = host_from_url("https://[::1]:8080/");
        assert_eq!(host.as_deref(), Some("::1"));
    }

    /// IDNA → Punycode, abschließender Punkt entfernt, prozentkodierter Host dekodiert.
    #[test]
    fn test_host_from_url_normalizes_like_whatwg() {
        let cases = [
            ("https://bücher.de/", "xn--bcher-kva.de"),
            ("https://Docs.RS./", "docs.rs"),
            ("https://%64ocs.rs/", "docs.rs"),
        ];
        for (input, expected) in cases {
            assert_eq!(host_from_url(input).as_deref(), Some(expected), "{input}");
        }
    }

    /// Ende-zu-Ende über den Guard: der Backslash-Trick kommt an einem auf
    /// `docs.rs` beschränkten Scope nicht mehr vorbei, ein absoluter Name mit
    /// abschließendem Punkt bleibt erlaubt.
    #[test]
    fn test_require_host_access_blocks_backslash_bypass() -> TestResult {
        let (_base, spec) = make_sandbox_with_hosts(
            "host_blocks_backslash_bypass",
            vec![Permission::NetworkAccess],
            vec!["docs.rs"],
        )?;
        let ctx = make_ctx(spec);

        let at_bypass =
            host_from_url("https://evil.com\\@docs.rs/").ok_or(TestError::Missing("Host"))?;
        let dot_bypass =
            host_from_url("https://evil.com\\.docs.rs/").ok_or(TestError::Missing("Host"))?;
        for host in [at_bypass, dot_bypass] {
            assert!(
                require_host_access(&ctx, &host, "http.fetch").is_some(),
                "{host:?} darf nicht als docs.rs durchgehen"
            );
        }

        let host = host_from_url("https://static.docs.rs./x").ok_or(TestError::Missing("Host"))?;
        assert!(require_host_access(&ctx, &host, "http.fetch").is_none());
        Ok(())
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
