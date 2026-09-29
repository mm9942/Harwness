//! `infra.*` — control-plane operations over the Harwness infrastructure
//! daemons (Crypto-Infrastructure masterplan v2 §11, §12, §34 H5).
//!
//! # Operations
//! | Name | Domain | Tier | Command | Web |
//! |---|---|---|---|---|
//! | `infra.status` | security | observer | `/infra-status` | `GET /api/infra/status` |
//! | `infra.health` | security | observer | `/infra-health` | `GET /api/infra/health` |
//! | `infra.auth.keys.describe` | crypto | observer | `/infra-key-describe` | `GET /api/infra/auth/keys/describe` |
//! | `infra.auth.keys.rotate` | crypto | owner | `/infra-key-rotate` | `POST /api/infra/auth/keys/rotate`, `approval = "always"` |
//!
//! Names follow the `infra.<area>.<noun-plural>.<verb>` convention of
//! `harw_operations::operation`.
//!
//! # No model surface
//! None of these operations declares a `model_tool` surface. The rule
//! (masterplan §11/§12, narrowed by the R18 contract D-B,
//! `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`):
//!
//! > A language model never drives key operations (`infra.auth.keys.*`) and
//! > never reaches `infra.*`. It may inspect the **gateway** through the
//! > read-only `gateway.*` model tools and may request gateway mutations through
//! > the mutating `gateway.*` model tools, each of which asks a human every time
//! > (`approval = "always"`). Approval never expands authority: the operation
//! > still runs with the caller's gateway caps and tenant scope.
//!
//! The `gateway.*` operations live in `crate::gateway_ops`. The operations
//! here are reachable only as operator commands and through the local Web
//! UI. The runtime additionally puts the
//! `Arc<InfrastructureAvailability>` service only on the Slash and Web
//! surfaces, never on the model-tool surface.
//!
//! # Service, never a socket
//! Every operation reads `Arc<harw_infra_client::InfrastructureAvailability>`
//! from the [`OpContext`]; none of them opens a socket itself (H4 exit
//! criterion). Without that service every operation answers
//! [`OpError::NotAvailable`].
//!
//! # Registration
//! These operations are **not** part of [`crate::register_all`]. The
//! runtime's `InfrastructureContributor` calls [`register_infrastructure`]
//! only when `[infrastructure]` is configured, so a runtime without
//! infrastructure does not even list them.
//!
//! # Secrets
//! Output contains socket paths, daemon-reported names/versions/states, key
//! references, key states, profile names and public-key *lengths* — never the
//! bearer token, never key material, never public-key bytes. Daemon-reported
//! text is stripped of control characters and truncated before it is shown.
//!
//! # Mutations and timeouts
//! `infra.auth.keys.rotate` is a mutation. On a timeout its outcome is
//! **unknown**; the error says so and points to `infra.auth.keys.describe`.
//! The operation never retries on its own.

use std::path::Path;
use std::sync::Arc;

use harw_infra_client::{
    AuthHubClient, Health, HealthState, InfraClientError, InfrastructureAvailability, KeyRef,
    KeyState, NetworkControlClient, SecurityHubClient, VersionInfo,
};
use harw_macros::operation;
use harw_operations::registry::{OperationRegistry, RegistryError};
use harw_operations::{OpContext, OpError, OpOutput, Operation};
use serde_json::{Value, json};

/// Number of operations [`register_infrastructure`] adds.
pub const INFRA_OP_COUNT: usize = 4;

/// Longest daemon-reported text shown verbatim (service name, version).
const MAX_REPORTED_TEXT: usize = 128;

const NOT_CONFIGURED: &str =
    "infrastructure is not configured (no usable [infrastructure] section)";

/// Registers the four `infra.*` operations.
///
/// # Description
/// Called by the runtime's `InfrastructureContributor` only when
/// `[infrastructure]` is configured. Uses `try_register`, so a second call on
/// the same registry reports the collision instead of panicking.
///
/// # Errors
/// The first [`RegistryError`] (name or web-route collision). Operations
/// registered before the collision stay registered.
///
/// # Example
/// ```rust
/// use harw_operations::registry::OperationRegistry;
///
/// # fn main() -> Result<(), harw_operations::registry::RegistryError> {
/// let mut registry = OperationRegistry::new();
/// let added = harw_ops::infra::register_infrastructure(&mut registry)?;
/// assert_eq!(added, harw_ops::infra::INFRA_OP_COUNT);
/// assert!(registry.find_by_name("infra.status").is_some());
/// assert!(registry.find_by_name("infra.auth.keys.rotate").is_some());
/// # Ok(())
/// # }
/// ```
pub fn register_infrastructure(registry: &mut OperationRegistry) -> Result<usize, RegistryError> {
    let ops: [Arc<dyn Operation>; INFRA_OP_COUNT] = [
        Arc::new(InfraStatusOperation),
        Arc::new(InfraHealthOperation),
        Arc::new(InfraAuthKeysDescribeOperation),
        Arc::new(InfraAuthKeysRotateOperation),
    ];
    for op in ops {
        registry.try_register(op)?;
    }
    Ok(INFRA_OP_COUNT)
}

// ── Arguments ────────────────────────────────────────────────────────────────

/// Arguments of `infra.status` (none).
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct InfraStatusArgs {}

/// Arguments of `infra.health`.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
pub struct InfraHealthArgs {
    /// Only this daemon: `auth`, `network` or `security`. Default: every
    /// configured daemon.
    #[serde(default)]
    #[raw(first)]
    pub daemon: Option<String>,
}

/// Key address of `infra.auth.keys.describe` / `infra.auth.keys.rotate`:
/// `namespace id [version]`.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
pub struct InfraKeyArgs {
    /// Key namespace (`[A-Za-z0-9._-]`, not starting with `.`).
    #[serde(default)]
    #[raw(first)]
    pub namespace: Option<String>,
    /// Key id within the namespace (same character rules).
    #[serde(default)]
    #[raw(nth = 1)]
    pub id: Option<String>,
    /// Optional key version (`>= 1`); absent means the latest version.
    #[serde(default)]
    #[raw(nth = 2)]
    pub version: Option<String>,
}

// ── Operations ───────────────────────────────────────────────────────────────

/// Shows which infrastructure daemons are configured and whether each is
/// reachable.
///
/// # Output
/// One line per daemon (`auth`, `network`, `security`): `not configured`, or
/// the socket path with `ok` / `degraded` / `unknown` / `unavailable (…)`.
/// A configured but missing socket is reported as unavailable, never as an
/// error of the operation. `data` carries the same as JSON.
///
/// # Errors
/// [`OpError::NotAvailable`] without the infrastructure service.
#[operation(
    name = "infra.status",
    summary = "Zeigt, welche Infrastruktur-Daemons konfiguriert und erreichbar sind.",
    domain = "security",
    permission = "observer",
    command(path = "/infra-status", visibility = "tui_only", busy = "immediate"),
    // Reine Abfrage (GET /v1/health je Daemon), keine Mutation.
    web(path = "/api/infra/status", method = "get", approval = "none")
)]
async fn infra_status(ctx: &OpContext, _args: InfraStatusArgs) -> Result<OpOutput, OpError> {
    let infra = availability(ctx)?;
    let (auth, network, security) = tokio::join!(
        probe_status(DaemonKind::Auth, infra.auth.as_ref().map(Daemon::Auth)),
        probe_status(
            DaemonKind::Network,
            infra.network.as_ref().map(Daemon::Network)
        ),
        probe_status(
            DaemonKind::Security,
            infra.security.as_ref().map(Daemon::Security)
        ),
    );
    let rows = [auth, network, security];
    let mut text = String::from("Infrastructure status:");
    for row in &rows {
        text.push('\n');
        text.push_str(&row.line);
    }
    let data = json!({ "daemons": rows.into_iter().map(|row| row.data).collect::<Vec<_>>() });
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

/// Health and version of each configured daemon.
///
/// # Errors
/// - [`OpError::NotAvailable`] without the infrastructure service, or if the
///   daemon named in `daemon` is not configured.
/// - [`OpError::InvalidArguments`] for an unknown `daemon` name.
#[operation(
    name = "infra.health",
    summary = "Zeigt Gesundheit und Version der konfigurierten Infrastruktur-Daemons.",
    domain = "security",
    permission = "observer",
    command(path = "/infra-health", visibility = "tui_only", busy = "immediate"),
    // Reine Abfrage (GET /v1/health, /v1/version), keine Mutation.
    web(path = "/api/infra/health", method = "get", approval = "none")
)]
async fn infra_health(ctx: &OpContext, args: InfraHealthArgs) -> Result<OpOutput, OpError> {
    let infra = availability(ctx)?;
    let filter = args.daemon.as_deref().map(DaemonKind::parse).transpose()?;
    let daemons = configured_daemons(infra);
    let selected: Vec<Daemon<'_>> = daemons
        .into_iter()
        .filter(|daemon| filter.is_none_or(|kind| daemon.kind() == kind))
        .collect();
    if selected.is_empty() {
        return Err(OpError::NotAvailable(match filter {
            Some(kind) => format!("the {} daemon is not configured", kind.as_str()),
            None => NOT_CONFIGURED.to_owned(),
        }));
    }
    let mut lines = Vec::with_capacity(selected.len());
    let mut entries = Vec::with_capacity(selected.len());
    for daemon in &selected {
        let (health, version) = tokio::join!(daemon.health(), daemon.version());
        let (line, data) = render_health(daemon, &health, &version);
        lines.push(line);
        entries.push(data);
    }
    Ok(OpOutput {
        text: lines.join("\n"),
        data: Some(json!({ "daemons": entries })),
    })
}

/// Metadata of one AuthHub key version (never key material).
///
/// # Errors
/// - [`OpError::NotAvailable`] without the infrastructure service or without
///   a configured AuthHub, or when the hub is unreachable.
/// - [`OpError::InvalidArguments`] for a missing/invalid namespace, id or
///   version.
/// - [`OpError::Execution`] for any other hub error.
#[operation(
    name = "infra.auth.keys.describe",
    summary = "Zeigt Metadaten eines AuthHub-Schlüssels (Zustand, Profil) — nie Schlüsselmaterial.",
    domain = "crypto",
    permission = "observer",
    command(path = "/infra-key-describe", visibility = "tui_only", busy = "immediate"),
    // Liest nur Metadaten (GET /v1/keys/{ns}/{id}), keine Mutation.
    web(path = "/api/infra/auth/keys/describe", method = "get", approval = "none")
)]
async fn infra_auth_keys_describe(
    ctx: &OpContext,
    args: InfraKeyArgs,
) -> Result<OpOutput, OpError> {
    let key = key_ref(&args)?;
    let auth = auth_client(ctx)?;
    let description = auth
        .describe(&key)
        .await
        .map_err(|error| map_client_error("AuthHub", error, false))?;
    let profile = description
        .profile_name
        .as_deref()
        .map(clean)
        .unwrap_or_else(|| "unnamed".to_owned());
    let state = key_state(description.state);
    Ok(OpOutput {
        text: format!("Key {}: state {state}, profile {profile}", description.key),
        data: Some(json!({
            "key": description.key.to_string(),
            "namespace": description.key.namespace(),
            "id": description.key.id(),
            "version": description.key.version(),
            "state": state,
            "profile": profile,
        })),
    })
}

/// Rotates an AuthHub key: creates its next version.
///
/// # Authority
/// Strongest tier (`owner`); on the Web surface `approval = "always"`, so
/// `harw-web` turns a call into an approval request rather than executing
/// it. No model-tool surface.
///
/// # Errors
/// As [`infra_auth_keys_describe`]; a timeout is reported as an
/// [`OpError::Execution`] stating that the outcome is unknown.
#[operation(
    name = "infra.auth.keys.rotate",
    summary = "Rotiert einen AuthHub-Schlüssel (neue Version). Irreversibel, nur für Owner.",
    domain = "crypto",
    permission = "owner",
    command(path = "/infra-key-rotate", visibility = "tui_only"),
    // Mutation: POST, und `approval = "always"` — eine Schlüsselrotation ist
    // nicht rückgängig zu machen und wird für `harw-web` zu einer
    // Genehmigungsanfrage, nie zu einem direkt ausführbaren Knopf.
    web(path = "/api/infra/auth/keys/rotate", method = "post", approval = "always")
)]
async fn infra_auth_keys_rotate(ctx: &OpContext, args: InfraKeyArgs) -> Result<OpOutput, OpError> {
    let key = key_ref(&args)?;
    let auth = auth_client(ctx)?;
    let created = auth
        .rotate(&key)
        .await
        .map_err(|error| map_client_error("AuthHub", error, true))?;
    let public_len = created
        .public
        .as_ref()
        .map(|public| public.as_bytes().len());
    let public_text = match public_len {
        Some(len) => format!("public key {len} bytes"),
        None => "no public key".to_owned(),
    };
    Ok(OpOutput {
        text: format!(
            "Rotated {key}: new version {} ({public_text}).",
            created.key
        ),
        data: Some(json!({
            "rotated": key.to_string(),
            "key": created.key.to_string(),
            "version": created.key.version(),
            "public_key_bytes": public_len,
        })),
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn availability(ctx: &OpContext) -> Result<&Arc<InfrastructureAvailability>, OpError> {
    ctx.service::<Arc<InfrastructureAvailability>>()
        .ok_or_else(|| OpError::NotAvailable(NOT_CONFIGURED.to_owned()))
}

fn auth_client(ctx: &OpContext) -> Result<&AuthHubClient, OpError> {
    availability(ctx)?.auth.as_ref().ok_or_else(|| {
        OpError::NotAvailable("the AuthHub socket (auth_socket) is not configured".to_owned())
    })
}

fn key_ref(args: &InfraKeyArgs) -> Result<KeyRef, OpError> {
    let namespace = required(args.namespace.as_deref(), "namespace")?;
    let id = required(args.id.as_deref(), "id")?;
    let key = match args
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        None => KeyRef::latest(namespace, id),
        Some(version) => {
            let version: u32 = version.parse().map_err(|_| {
                OpError::InvalidArguments("version must be a positive integer".to_owned())
            })?;
            KeyRef::versioned(namespace, id, version)
        }
    };
    key.map_err(|error| OpError::InvalidArguments(error.to_string()))
}

fn required<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, OpError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::InvalidArguments(format!("{field} is required")))
}

/// Maps a client error to an operation error without any payload.
fn map_client_error(daemon: &str, error: InfraClientError, mutation: bool) -> OpError {
    match error {
        InfraClientError::Unavailable => {
            OpError::NotAvailable(format!("the {daemon} daemon is unavailable"))
        }
        InfraClientError::NotFound => {
            OpError::Execution("key not found (or not visible to this caller)".to_owned())
        }
        InfraClientError::Timeout if mutation => OpError::Execution(format!(
            "{daemon} call timed out; the outcome is unknown — check with \
             infra.auth.keys.describe before retrying"
        )),
        other => OpError::Execution(other.to_string()),
    }
}

fn key_state(state: KeyState) -> String {
    match state {
        KeyState::Enabled => "enabled".to_owned(),
        KeyState::Disabled => "disabled".to_owned(),
        KeyState::PendingDestruction => "pending-destruction".to_owned(),
        KeyState::Destroyed => "destroyed".to_owned(),
        KeyState::Unknown(code) => format!("unknown({code})"),
        _ => "unknown".to_owned(),
    }
}

fn health_state(state: HealthState) -> &'static str {
    match state {
        HealthState::Ok => "ok",
        HealthState::Degraded => "degraded",
        _ => "unknown",
    }
}

/// Daemon-reported text without control characters, bounded in length.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_REPORTED_TEXT)
        .collect()
}

fn socket_text(path: &Path) -> String {
    clean(&path.display().to_string())
}

/// The three daemon kinds, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DaemonKind {
    Auth,
    Network,
    Security,
}

impl DaemonKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Network => "network",
            Self::Security => "security",
        }
    }

    fn parse(value: &str) -> Result<Self, OpError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auth" | "authhub" | "auth-hub" => Ok(Self::Auth),
            "network" | "netsec" => Ok(Self::Network),
            "security" | "securityhub" | "security-hub" => Ok(Self::Security),
            _ => Err(OpError::InvalidArguments(
                "daemon must be one of: auth, network, security".to_owned(),
            )),
        }
    }
}

/// A configured daemon client. The clients share no trait; this enum is the
/// common view over their identical health/version surface.
#[derive(Clone, Copy)]
enum Daemon<'a> {
    Auth(&'a AuthHubClient),
    Network(&'a NetworkControlClient),
    Security(&'a SecurityHubClient),
}

impl Daemon<'_> {
    fn kind(&self) -> DaemonKind {
        match self {
            Self::Auth(_) => DaemonKind::Auth,
            Self::Network(_) => DaemonKind::Network,
            Self::Security(_) => DaemonKind::Security,
        }
    }

    fn socket_path(&self) -> &Path {
        match self {
            Self::Auth(client) => client.socket_path(),
            Self::Network(client) => client.socket_path(),
            Self::Security(client) => client.socket_path(),
        }
    }

    async fn health(&self) -> Result<Health, InfraClientError> {
        match self {
            Self::Auth(client) => client.health().await,
            Self::Network(client) => client.health().await,
            Self::Security(client) => client.health().await,
        }
    }

    async fn version(&self) -> Result<VersionInfo, InfraClientError> {
        match self {
            Self::Auth(client) => client.version().await,
            Self::Network(client) => client.version().await,
            Self::Security(client) => client.version().await,
        }
    }
}

fn configured_daemons(infra: &InfrastructureAvailability) -> Vec<Daemon<'_>> {
    let mut daemons = Vec::with_capacity(3);
    if let Some(client) = &infra.auth {
        daemons.push(Daemon::Auth(client));
    }
    if let Some(client) = &infra.network {
        daemons.push(Daemon::Network(client));
    }
    if let Some(client) = &infra.security {
        daemons.push(Daemon::Security(client));
    }
    daemons
}

/// One rendered `infra.status` row.
struct StatusRow {
    line: String,
    data: Value,
}

async fn probe_status(kind: DaemonKind, daemon: Option<Daemon<'_>>) -> StatusRow {
    let name = kind.as_str();
    let Some(daemon) = daemon else {
        return StatusRow {
            line: format!("{name:<9}not configured"),
            data: json!({ "daemon": name, "configured": false }),
        };
    };
    let socket = socket_text(daemon.socket_path());
    match daemon.health().await {
        Ok(health) => {
            let state = health_state(health.state);
            StatusRow {
                line: format!("{name:<9}{socket}  {state}"),
                data: json!({
                    "daemon": name,
                    "configured": true,
                    "socket": socket,
                    "state": state,
                }),
            }
        }
        Err(error) => StatusRow {
            line: format!("{name:<9}{socket}  unavailable ({error})"),
            data: json!({
                "daemon": name,
                "configured": true,
                "socket": socket,
                "state": "unavailable",
                "error": error.to_string(),
            }),
        },
    }
}

fn render_health(
    daemon: &Daemon<'_>,
    health: &Result<Health, InfraClientError>,
    version: &Result<VersionInfo, InfraClientError>,
) -> (String, Value) {
    let name = daemon.kind().as_str();
    let socket = socket_text(daemon.socket_path());
    let (health_text, health_data) = match health {
        Ok(health) => {
            let state = health_state(health.state);
            let service = health.service.as_deref().map(clean);
            (
                state.to_owned(),
                json!({ "state": state, "service": service }),
            )
        }
        Err(error) => (
            format!("unavailable ({error})"),
            json!({ "state": "unavailable", "error": error.to_string() }),
        ),
    };
    let (version_text, version_data) = match version {
        Ok(info) => {
            let service = clean(&info.service);
            let version = clean(&info.version);
            (
                format!("{service} {version} (protocol {})", info.protocol),
                json!({ "service": service, "version": version, "protocol": info.protocol }),
            )
        }
        Err(error) => (
            format!("version unavailable ({error})"),
            json!({ "error": error.to_string() }),
        ),
    };
    (
        format!("{name:<9}{socket}  health: {health_text}; {version_text}"),
        json!({
            "daemon": name,
            "socket": socket,
            "health": health_data,
            "version": version_data,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        InfraAuthKeysDescribeOperation, InfraAuthKeysRotateOperation, InfraHealthOperation,
        InfraKeyArgs, InfraStatusOperation, register_infrastructure,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_infra_client::{InfraClientConfig, InfrastructureAvailability};
    use harw_operations::registry::OperationRegistry;
    use harw_operations::{
        ApprovalPolicy, FromRawArgs, OpContext, OpError, OpInput, Operation, OperationDomain,
        PermissionTier, Surface, context::ServiceMap,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::Path;
    use std::sync::Arc;

    fn op_context(
        root: &Path,
        infra: Option<Arc<InfrastructureAvailability>>,
    ) -> TestResult<OpContext> {
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("create workspace"))?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("ws"),
                root: "ws".into(),
            }],
        )
        .map_err(ctx("workspace registry"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("ws"))
            .map_err(ctx("resolve workspace"))?;
        let mut services = ServiceMap::new();
        if let Some(infra) = infra {
            services.insert(infra);
        }
        Ok(OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(binding, PermissionSet::empty()),
            services,
        ))
    }

    fn key_json() -> serde_json::Value {
        serde_json::json!({ "namespace": "app", "id": "k1" })
    }

    fn all_ops() -> [(Arc<dyn Operation>, serde_json::Value); 4] {
        [
            (Arc::new(InfraStatusOperation), serde_json::Value::Null),
            (Arc::new(InfraHealthOperation), serde_json::Value::Null),
            (Arc::new(InfraAuthKeysDescribeOperation), key_json()),
            (Arc::new(InfraAuthKeysRotateOperation), key_json()),
        ]
    }

    #[tokio::test]
    async fn every_infra_op_is_not_available_without_the_service() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let op_ctx = op_context(dir.path(), None)?;
        for (op, args) in all_ops() {
            let result = op.run(&op_ctx, OpInput::model_tool(args)).await;
            assert!(
                matches!(result, Err(OpError::NotAvailable(_))),
                "{} must be NotAvailable without infrastructure: {result:?}",
                op.meta().name
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn status_reports_a_configured_but_missing_socket_as_unavailable() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let missing = dir.path().join("missing-secure.sock");
        let infra = InfrastructureAvailability::from_config(&InfraClientConfig {
            auth_socket: Some(missing.clone()),
            ..InfraClientConfig::default()
        })
        .map_err(ctx("build availability"))?;
        let op_ctx = op_context(dir.path(), Some(Arc::new(infra)))?;

        let output = InfraStatusOperation
            .run(&op_ctx, OpInput::model_tool(serde_json::Value::Null))
            .await
            .map_err(ctx("infra.status"))?;
        let auth_line = output
            .text
            .lines()
            .find(|line| line.starts_with("auth"))
            .ok_or(TestError::Missing("auth line"))?;
        assert!(auth_line.contains("unavailable"), "{}", output.text);
        assert!(
            auth_line.contains(&missing.display().to_string()),
            "{}",
            output.text
        );
        assert!(
            output.text.contains("network  not configured"),
            "{}",
            output.text
        );
        assert!(
            output.text.contains("security not configured"),
            "{}",
            output.text
        );

        let data = output.data.ok_or(TestError::Missing("status data"))?;
        assert_eq!(data["daemons"][0]["state"], "unavailable");
        assert_eq!(data["daemons"][1]["configured"], false);
        Ok(())
    }

    #[tokio::test]
    async fn key_ops_are_not_available_without_an_auth_socket() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let infra = InfrastructureAvailability::from_config(&InfraClientConfig {
            network_socket: Some(dir.path().join("network.sock")),
            ..InfraClientConfig::default()
        })
        .map_err(ctx("build availability"))?;
        let op_ctx = op_context(dir.path(), Some(Arc::new(infra)))?;
        let result = InfraAuthKeysDescribeOperation
            .run(&op_ctx, OpInput::model_tool(key_json()))
            .await;
        assert!(
            matches!(&result, Err(OpError::NotAvailable(reason)) if reason.contains("auth_socket")),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn describe_against_a_missing_socket_is_not_available_and_leaks_no_token() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let token_path = dir.path().join("auth.token");
        std::fs::write(&token_path, b"infra-op-secret-token\n").map_err(ctx("write token"))?;
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))
            .map_err(ctx("chmod token"))?;
        let infra = InfrastructureAvailability::from_config(&InfraClientConfig {
            auth_socket: Some(dir.path().join("missing-secure.sock")),
            token_file: Some(token_path),
            ..InfraClientConfig::default()
        })
        .map_err(ctx("build availability"))?;
        let op_ctx = op_context(dir.path(), Some(Arc::new(infra)))?;

        let result = InfraAuthKeysDescribeOperation
            .run(&op_ctx, OpInput::model_tool(key_json()))
            .await;
        match &result {
            Err(OpError::NotAvailable(reason)) => {
                assert!(!reason.contains("infra-op-secret-token"), "{reason}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }

        let status = InfraStatusOperation
            .run(&op_ctx, OpInput::model_tool(serde_json::Value::Null))
            .await
            .map_err(ctx("infra.status"))?;
        assert!(
            !status.text.contains("infra-op-secret-token"),
            "{}",
            status.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn key_args_are_validated_before_any_call() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let infra = InfrastructureAvailability::from_config(&InfraClientConfig {
            auth_socket: Some(dir.path().join("secure.sock")),
            ..InfraClientConfig::default()
        })
        .map_err(ctx("build availability"))?;
        let op_ctx = op_context(dir.path(), Some(Arc::new(infra)))?;
        for args in [
            serde_json::json!({ "namespace": "app" }),
            serde_json::json!({ "namespace": "a/b", "id": "k" }),
            serde_json::json!({ "namespace": "app", "id": "k", "version": "0" }),
            serde_json::json!({ "namespace": "app", "id": "k", "version": "x" }),
        ] {
            let result = InfraAuthKeysRotateOperation
                .run(&op_ctx, OpInput::model_tool(args.clone()))
                .await;
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "{args}: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn key_args_parse_from_command_tokens() -> TestResult {
        let args = InfraKeyArgs::from_raw_args(&toks(&["app", "k1", "3"]))
            .map_err(ctx("from_raw_args"))?;
        assert_eq!(args.namespace.as_deref(), Some("app"));
        assert_eq!(args.id.as_deref(), Some("k1"));
        assert_eq!(args.version.as_deref(), Some("3"));
        Ok(())
    }

    #[test]
    fn surfaces_domains_and_tiers_are_as_declared() -> TestResult {
        let mut registry = OperationRegistry::new();
        let added = register_infrastructure(&mut registry).map_err(ctx("register"))?;
        assert_eq!(added, super::INFRA_OP_COUNT);
        assert_eq!(registry.len(), super::INFRA_OP_COUNT);

        for op in registry.iter() {
            let meta = op.meta();
            assert!(
                !meta.surfaces.iter().any(|surface| matches!(
                    surface,
                    Surface::ModelTool { .. } | Surface::AgentTool { .. }
                )),
                "{} must not be reachable by a model",
                meta.name
            );
        }

        let rotate = registry
            .find_by_name("infra.auth.keys.rotate")
            .ok_or(TestError::Missing("rotate"))?;
        assert_eq!(rotate.meta().domain, OperationDomain::Crypto);
        assert_eq!(rotate.meta().permission, PermissionTier::Owner);
        assert!(rotate.meta().surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Web {
                method: harw_operations::operation::WebMethod::Post,
                approval: ApprovalPolicy::Always,
                ..
            }
        )));

        let status = registry
            .find_by_name("infra.status")
            .ok_or(TestError::Missing("status"))?;
        assert_eq!(status.meta().domain, OperationDomain::Security);
        assert_eq!(status.meta().permission, PermissionTier::Observer);

        // Second registration collides instead of panicking.
        assert!(register_infrastructure(&mut registry).is_err());
        Ok(())
    }
}
