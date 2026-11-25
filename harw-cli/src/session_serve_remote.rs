//! Composition root of the own-cloud (remote) session ingress.
//!
//! # Description
//! The counterpart of [`crate::session_serve`] for remote devices: a node
//! transport server (mutual, pinned ML-DSA-65 handshake) plus the
//! `harw-node-listener` upgrade handler on a TCP listener. Composition only;
//! identity rules live in `harw-node-listener`, session rules in
//! `harw-session-host`.
//!
//! ```text
//! start_remote(config, model, LocalNode, verifier, TcpListener)
//!   |-- SessionHost  own records    <profile>/session-host/remote/host
//!   |-- transcripts  share off:     <profile>/session-host/remote/sessions
//!   |                share on:      <profile>/sessions (the TUI's store)
//!   |-- driver       CoreTurnDriver + GatewayCoreFactory (tier = config.tier)
//!   |-- identity     RegistryIdentityMapper (<state>/remote/node-devices.conf)
//!   `-- listener     NodeListener::serve (revocation closes live connections)
//! ```
//!
//! # Why a separate host
//! A hosted session's rights come from the factory, which does not see who
//! created the session. The local socket's peer is the gateway's own uid
//! (an operator); remote devices are different principals. They therefore get
//! their **own host, records and factory**: the sandbox rights are cut to
//! [`RemoteServeConfig::tier`] (default observer), and a device's registry tier
//! only decides its host caps.
//!
//! # Sharing the TUI's sessions
//! [`RemoteServeConfig::share`] (`[session_listener] share_profile_sessions`,
//! default `off`) decides whether remote devices see the profile's own
//! sessions. With `off` the remote host keeps its private transcript store and
//! nothing of the profile is visible. With `read` or `continue` the transcript
//! and approval store is the profile store `<profile>/sessions`, the core
//! writes with the `cli-session:<id>` thread (the same one `harw` uses, so a
//! continued TUI session sees its earlier entries), a read-only
//! [`crate::profile_index::FsProfileIndex`] lists the profile's sessions and
//! projects for the host, and every session runs in its checked working
//! directory (`project_root`/`cwd` of its meta, only inside the known project
//! roots, narrowed by `share_roots`). Host records stay in the remote
//! directory. Leases, meta writes on adopt and turn end, and the read/continue
//! gates are the host's (`harw-session-host`); this module only composes.
//! Startup tail repair runs for the host's own record ids only, never for all
//! profile sessions (another process may be writing them).
//!
//! # Approvals
//! A parked approval is bound to one actor. Remote sessions bind it to
//! [`RemoteServeConfig::approval_device`] (`device:<id>`), which must also be
//! enrolled with the `approve` opt-in in the device registry. Without it the
//! actor is a placeholder nobody carries and a tool needing approval parks
//! forever (fail closed): a remote tier or key never grants approval by itself.
//!
//! # Wiring
//! `harw gateway` starts this from `[session_listener]` (global-only, default
//! off) via `crate::session_listener::start_from_config`: the node signer is
//! the AuthHub key `harw.node-identity/<node_id>`, trust comes from
//! `node-peers.conf`, authorisation from `node-devices.conf`. This module
//! itself takes the node, verifier and listener as parameters so the chain is
//! tested over a real loopback socket with in-process keys.
//!
//! # Optional direct owner status
//! `HARW_DIRECT_OWNER_STATUS_INSTANCE` is a startup-only operator setting.
//! When absent, direct owner status is not offered. Its validated value labels
//! this SessionHost instance; it is not a process key or remote Hub proof.
//! Bootstrap pins the actual AuthHub node and the newly opened host's native
//! epoch. The independent direct listener still verifies the exact originating
//! device, key, current membership and creator-bound session on every read.
//! The optional `direct-owner-placement` build feature and explicit
//! `HARW_DIRECT_OWNER_PLACEMENT=1` additionally require private
//! `owner-placement-hub.toml` in the remote ingress directory:
//! its independently selected Hub `onion_locator` (v3 service with port)
//! or explicit `loopback_address` (literal loopback SocketAddr, laboratory only),
//! `node_id` and `public_key_hex` plus the process's `owner_epoch` and separate
//! `process_public_key_hex` (both public keys lowercase hex). The operator
//! supplies the matching one-incarnation 32-byte ML-DSA-65 process seed
//! as the fixed private systemd credential `harw-owner-observe-ml-dsa-65`.
//! The Hub must be pinned in the actual node transport but is not a device.
//! Onion requires the actual owner Onion ingress; its backing TCP listener
//! will not issue proof reservations or serve proofs. No route fallback.
//! The dedicated Arti client bootstraps independently of scoped requests;
//! an unready client or an exceeded proof deadline fails closed.
//! An optional bounded list of `[[report_assignments]]` names already
//! authorized exact `tenant`, `session` and Hub `generation` rows for this
//! configured incarnation. The local host record is verified before
//! sending; the Hub must still have the independently provisioned row.
//! Omit the list to send no report. Nothing auto-assigns or migrates ownership.
//!
//! # Isolated private remote writer (Linux, opt-in)
//!
//! `HARW_PRIVATE_REMOTE_WRITER=1` selects a new remote-only namespace under
//! `<profile>/session-host/remote/private-writer-v1`. The fixed writer lock is
//! held from before recovery until process exit, including listener shutdown.
//! It guards hosted records, transcripts and approvals, not other profile
//! stores such as media. A pre-existing unmarked or partially initialized
//! directory fails closed. No automatic import, legacy migration, cloud
//! custody, provider linking, sharing or Hub status/report is enabled.
//! This is local exclusion for cooperating processes in a protected deployment,
//! not proof that another account, host or legacy writer has been fenced.
//!
//! # Live owner policy
//!
//! Registry rights and retained creator identities are revalidated every
//! `revocation_poll` (default five seconds). Device removal, ambiguous or
//! unreadable policy invalidates existing authority without banning a future
//! valid authentication. Explicit `revoked` rows remain permanent host bans.
//! Peer pins are bound to the verifier's exact owner file at startup. Any pin
//! file edit or read failure cuts retained authority and blocks authentication
//! until the ingress restarts with verified fresh pins.
//!
//! # Jobs rule
//! Everything on the serve path is async on the caller's runtime.

use std::collections::HashSet;
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use harw_core::ModelProvider;
use harw_node_listener::{
    DeviceRecord, DeviceRegistry, HostRevoker, IdentityMapper, NodeListener,
    RegistryIdentityMapper, RevocationSink,
};
use harw_node_transport::{LocalNode, NodeTransportServer, NodeVerifier, ServerOptions};
use harw_session_daemon::DaemonServices;
use harw_session_driver::{CoreDriverConfig, CoreTurnDriver};
use harw_session_host::driver::TurnDriver;
use harw_session_host::index::{ProfileIndex, ProfileShare, ShareMode, ShareSettings};
use harw_session_host::{DurableApprovals, DurableTranscripts, HostConfig, SessionHost, ToolHost};
use harw_session_ws::WsLimits;
use harw_types::{ApprovalActor, DeviceId, NodeId, PermissionTier};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::profile_index::{FsProfileIndex, IndexOptions};
use crate::session_listener::SharePlan;
use crate::session_serve::{GatewayCoreFactory, SessionCwdResolver, private_dir};

#[cfg(all(unix, feature = "direct-owner-placement"))]
mod owner_placement;

#[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
mod writer_namespace;

#[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
mod writer_creation;

#[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
mod writer_readiness_lane;

/// Directory below the profile's session-host state that belongs to the
/// remote ingress.
const REMOTE_DIR_NAME: &str = "remote";

/// The remote ingress state directory of the active profile:
/// `<profile>/session-host/remote/` (where `node-devices.conf` and
/// `node-peers.conf` live). Resolved exactly as `harw gateway` does; nothing
/// is created.
///
/// # Errors
/// An invalid active profile name.
pub(crate) fn remote_state_dir(home: &Path) -> Result<PathBuf, String> {
    let profile = harw_home::active_profile_name(home);
    let dir = harw_home::profile_dir(home, &profile)
        .map_err(|error| format!("Profilverzeichnis '{profile}' nicht auflösbar: {error}"))?;
    Ok(dir
        .join(crate::session_serve::STATE_DIR_NAME)
        .join(REMOTE_DIR_NAME))
}
/// Export exact historical creation evidence for the active profile, locally.
#[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
pub(crate) fn export_private_writer_creation(home: &Path, tenant: &str, session: &str) -> Result<(), String> {
    use std::io::Write as _;
    let remote = remote_state_dir(home)?;
    let tenant = harw_types::TenantId::parse(tenant).map_err(|_| "invalid tenant")?;
    let session = harw_types::SessionId::parse(session).map_err(|_| "invalid session")?;
    let receipt = writer_namespace::read_committed(&remote, |state, namespace| {
        let receipt = harw_session_host::owner_creation::read_owner_creation_receipt(state, &tenant, &session)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "no matching durable row and creation receipt for the exact session"))?;
        if receipt.claims().namespace_id != namespace {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "receipt belongs to a different namespace"));
        }
        Ok(receipt)
    }).map_err(|error| format!("private writer creation evidence unavailable: {error}"))?;
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &receipt).map_err(|error| format!("creation receipt output: {error}"))?;
    output.write_all(b"\n").map_err(|error| format!("creation receipt output: {error}"))
}

/// Hosted sessions' transcripts and approvals below the remote directory.
const SESSIONS_DIR_NAME: &str = "sessions";
/// Actor of remote sessions when no approving device is configured: no
/// connection ever carries it.
const NO_APPROVER: &str = "remote:no-approver";

/// Where the remote ingress keeps its state and what it may do.
#[derive(Debug, Clone)]
pub struct RemoteServeConfig {
    /// `<profile>/session-host` (the remote ingress uses `<state_dir>/remote`).
    pub state_dir: PathBuf,
    /// The harw home hosted sessions assemble from.
    pub home: PathBuf,
    /// Workspace hosted sessions are bound to.
    pub cwd: PathBuf,
    /// Tier the remote sessions' sandbox rights are cut to. Default observer.
    pub tier: PermissionTier,
    /// The one device allowed to resolve approvals of remote sessions (it must
    /// also carry the registry `approve` opt-in). `None`: approvals park.
    pub approval_device: Option<DeviceId>,
    /// Device permitted to link the gateway provider account.
    pub provider_link_device: Option<DeviceId>,
    /// How often registry and pin policy is revalidated (live invalidation).
    /// Default 5 s; values below 10 ms are clamped to 10 ms.
    pub revocation_poll: Duration,
    /// Sharing of the profile's own sessions (`[session_listener]
    /// share_*`). Default off: the remote host keeps its private store.
    pub share: SharePlan,
    /// Optional, operator-configured custody paths. Snapshotted by the gateway
    /// launcher; never supplied by a session client.
    pub(crate) cloud_keys: CloudServiceKeys,
    /// Digest of the exact owner pin file used to build the transport verifier.
    pub(crate) peer_pins_digest: Option<[u8; 32]>,
    /// Explicit owner-process label from the trusted service environment.
    pub(crate) direct_owner_status_instance: Option<DirectOwnerStatusInstance>,
    /// Explicit scoped owner-to-Hub verifier selection. Default off.
    pub(crate) direct_owner_placement: bool,
    /// Opt in to an isolated, process-guarded private remote writer store.
    pub(crate) private_remote_writer: bool,
    /// Explicit startup opt-in to dedicated evidence for fresh private creates.
    pub(crate) private_writer_creation_receipts: bool,
    /// Separate, opted-in co-located Unix proof-only endpoint.
    pub(crate) private_writer_readiness: bool,
    /// The launcher must have selected the actual Onion inlet for owner proof.
    #[cfg(all(unix, feature = "direct-owner-placement"))]
    pub(crate) owner_onion_ingress: bool,
    #[cfg(test)]
    injected_peer_trust: bool,
}

/// Owner startup configuration; never taken from a peer, request, or report.
#[derive(Clone, Debug)]
pub(crate) struct DirectOwnerStatusInstance(String);

impl DirectOwnerStatusInstance {
    pub(crate) fn from_gateway_environment() -> Result<Option<Self>, String> {
        const ENV: &str = "HARW_DIRECT_OWNER_STATUS_INSTANCE";
        let Some(raw) = std::env::var_os(ENV) else {
            return Ok(None);
        };
        let valid = raw.to_str().filter(|value| {
            !value.is_empty() && value.len() <= 255 && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        });
        let Some(value) = valid else {
            return Err(format!("{ENV} must be a nonempty owner instance label (max 255 ASCII letters, digits, -, _, . or :)"));
        };
        Ok(Some(Self(value.to_owned())))
    }
}

pub(crate) fn direct_owner_placement_requested() -> Result<bool, String> {
    match std::env::var_os("HARW_DIRECT_OWNER_PLACEMENT") {
        None => Ok(false),
        Some(value) if value.to_str() == Some("0") => Ok(false),
        Some(value) if value.to_str() == Some("1") && cfg!(all(unix, feature = "direct-owner-placement")) => Ok(true),
        Some(value) if value.to_str() == Some("1") => Err("direct owner placement needs a build with the direct-owner-placement feature".into()),
        Some(_) => Err("HARW_DIRECT_OWNER_PLACEMENT must be 0 or 1".into()),
    }
}

#[derive(Clone, Default)]
pub(crate) struct CloudServiceKeys {
    library: Option<PathBuf>,
    assistant: Option<PathBuf>,
}

impl std::fmt::Debug for CloudServiceKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudServiceKeys")
            .field("library_configured", &self.library.is_some())
            .field("assistant_configured", &self.assistant.is_some())
            .finish()
    }
}

impl CloudServiceKeys {
    pub(crate) fn from_gateway_environment() -> Self {
        Self {
            library: std::env::var_os("HARW_CLOUD_LIBRARY_KEY_FILE").map(PathBuf::from),
            assistant: std::env::var_os("HARW_CLOUD_ASSISTANT_KEY_FILE").map(PathBuf::from),
        }
    }
}

struct AccountLink {
    owner: DeviceId,
    home: PathBuf,
}

impl harw_node_listener::ProviderLink for AccountLink {
    fn handle<'a>(
        &'a self,
        identity: harw_session_host::ClientIdentity,
        method: http::Method,
        body: bytes::Bytes,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = http::StatusCode> + Send + 'a>> {
        Box::pin(async move {
            use harw_oauth::chatgpt_plan::{ChatGptPlanError, ChatGptPlanStore};
            use http::StatusCode;
            if identity.device.as_ref() != Some(&self.owner) {
                return StatusCode::FORBIDDEN;
            }
            let store = ChatGptPlanStore::new(&self.home);
            let result = match method {
                http::Method::GET if body.is_empty() => store.linked().await.map(|linked| {
                    if linked {
                        StatusCode::NO_CONTENT
                    } else {
                        StatusCode::NOT_FOUND
                    }
                }),
                http::Method::DELETE if body.is_empty() => {
                    store.disconnect().await.map(|confirmed| {
                        if confirmed {
                            StatusCode::NO_CONTENT
                        } else {
                            StatusCode::ACCEPTED
                        }
                    })
                }
                http::Method::POST => {
                    let Ok(record) = serde_json::from_slice(&body) else {
                        return StatusCode::BAD_REQUEST;
                    };
                    store
                        .store_verified(&record)
                        .await
                        .map(|()| StatusCode::NO_CONTENT)
                }
                _ => return StatusCode::BAD_REQUEST,
            };
            result.unwrap_or_else(|error| match error {
                ChatGptPlanError::DifferentRegistration => StatusCode::CONFLICT,
                ChatGptPlanError::OpenAiUnavailable => StatusCode::SERVICE_UNAVAILABLE,
                ChatGptPlanError::Storage => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            })
        })
    }
}

pub(crate) fn private_remote_writer_requested() -> Result<bool, String> {
    const ENV: &str = "HARW_PRIVATE_REMOTE_WRITER";
    match std::env::var_os(ENV) {
        None => Ok(false),
        Some(value) if value.to_str() == Some("0") => Ok(false),
        Some(value) if value.to_str() == Some("1") && cfg!(all(target_os = "linux", feature = "direct-owner-placement")) => Ok(true),
        Some(value) if value.to_str() == Some("1") =>
            Err(format!("{ENV} needs Linux and the direct-owner-placement build feature")),
        Some(_) => Err(format!("{ENV} must be 0 or 1")),
    }
}

pub(crate) fn private_writer_creation_receipts_requested() -> Result<bool, String> {
    const ENV: &str = "HARW_PRIVATE_WRITER_CREATE_RECEIPTS";
    match std::env::var_os(ENV) {
        None => Ok(false),
        Some(value) if value.to_str() == Some("0") => Ok(false),
        Some(value) if value.to_str() == Some("1") && cfg!(all(target_os = "linux", feature = "direct-owner-placement")) => Ok(true),
        Some(value) if value.to_str() == Some("1") =>
            Err(format!("{ENV} needs Linux and the direct-owner-placement build feature")),
        Some(_) => Err(format!("{ENV} must be 0 or 1")),
    }
}

pub(crate) fn private_writer_readiness_requested() -> Result<bool, String> {
    const ENV: &str = "HARW_PRIVATE_WRITER_READINESS";
    match std::env::var_os(ENV) {
        None => Ok(false),
        Some(value) if value.to_str() == Some("0") => Ok(false),
        Some(value) if value.to_str() == Some("1") && cfg!(all(target_os = "linux", feature = "direct-owner-placement")) => Ok(true),
        Some(value) if value.to_str() == Some("1") =>
            Err(format!("{ENV} needs Linux and the direct-owner-placement build feature")),
        Some(_) => Err(format!("{ENV} must be 0 or 1")),
    }
}

impl RemoteServeConfig {
    /// Conservative defaults: observer sandbox, nobody approves.
    #[must_use]
    pub fn new(state_dir: &Path, home: &Path, cwd: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
            tier: PermissionTier::Observer,
            approval_device: None,
            provider_link_device: None,
            revocation_poll: Duration::from_secs(5),
            share: SharePlan::off(),
            cloud_keys: CloudServiceKeys::default(),
            peer_pins_digest: None,
            direct_owner_status_instance: None,
            direct_owner_placement: false,
            private_remote_writer: false,
            private_writer_creation_receipts: false,
            private_writer_readiness: false,
            #[cfg(all(unix, feature = "direct-owner-placement"))]
            owner_onion_ingress: false,
            #[cfg(test)]
            injected_peer_trust: false,
        }
    }

    fn remote_dir(&self) -> PathBuf {
        self.state_dir.join(REMOTE_DIR_NAME)
    }

    fn actor(&self) -> ApprovalActor {
        let id = self.approval_device.as_ref().map_or_else(
            || NO_APPROVER.to_owned(),
            |device| format!("device:{}", device.as_str()),
        );
        ApprovalActor::Operator { id }
    }
}

/// The profile store shared with remote devices (`share.mode != off`).
struct SharedProfile {
    /// `<profile>/sessions` (transcripts, metas, approvals, leases).
    sessions_root: PathBuf,
    index: Arc<FsProfileIndex>,
    settings: ShareSettings,
}

/// Resolves the shared profile store of `config`; `None` when sharing is off.
///
/// # Errors
/// Sharing without a tenant (never a panic, although `SharePlan` validation
/// already rejects it) or an unresolvable profile sessions directory.
fn open_share(config: &RemoteServeConfig) -> Result<Option<SharedProfile>, String> {
    let share = &config.share;
    if matches!(share.mode, ShareMode::Off) {
        return Ok(None);
    }
    let tenant = share.tenant.clone().ok_or_else(|| {
        "[session_listener] share_tenant fehlt für share_profile_sessions".to_owned()
    })?;
    let sessions_root = crate::runtime_entry::profile_sessions_root(&config.home)?;
    let index = Arc::new(FsProfileIndex::open(
        sessions_root.clone(),
        IndexOptions {
            since: share.since,
            share_roots: share.roots.clone(),
            new_project_root: share.new_project_root.clone(),
        },
    ));
    Ok(Some(SharedProfile {
        sessions_root,
        index,
        settings: ShareSettings {
            mode: share.mode,
            tenant,
            legacy_rows: share.legacy_rows,
            listener_tier: config.tier,
        },
    }))
}

/// Working directory of a shared session: the index's checked
/// `project_root`/`cwd`. `None` (unknown, outside the known roots, lookup
/// error) falls back to the factory's default `cwd`; the host itself refuses
/// to continue such a session. Paths are never logged.
fn workdir_resolver(index: Arc<dyn ProfileIndex>) -> SessionCwdResolver {
    Arc::new(move |id: &harw_types::SessionId| match index.workdir(id) {
        Ok(dir) => dir,
        Err(_) => {
            tracing::warn!(session = %id, "profile index: working directory lookup failed");
            None
        }
    })
}

/// Owner policy files are bounded regular files and are never followed through
/// symlinks. Pin changes require restart because the verifier is immutable.
const POLICY_MAX_BYTES: usize = 1024 * 1024;

pub(crate) fn read_peer_pins(path: &Path) -> Result<String, String> {
    let file = harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::read_only())
        .map_err(|error| format!("remote peer pins unavailable: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > POLICY_MAX_BYTES as u64 {
        return Err("remote peer pins exceed their read limit or are not a regular file".into());
    }
    let mut text = String::new();
    file.take(POLICY_MAX_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() > POLICY_MAX_BYTES {
        return Err("remote peer pins exceed their read limit".into());
    }
    validate_peer_pin_text(text)
}

/// Stronger, path-bound trust source for the optional two-peer status command.
/// Ordinary attach retains its existing policy-file compatibility.
#[cfg(feature = "onion")]
pub(crate) fn read_private_peer_pins(directory: &Path) -> Result<String, String> {
    use std::fs::File;
    use std::os::{fd::AsFd as _, unix::fs::MetadataExt as _};
    let unavailable = || "Fenced owner status: private profile peer pins unavailable".to_owned();
    let dir = File::from(harw_fsutil::open_dir_nofollow(directory).map_err(|_| unavailable())?);
    harw_fsutil::ensure_owned_directory(&dir).map_err(|_| unavailable())?;
    if dir.metadata().map_err(|_| unavailable())?.mode() & 0o077 != 0 {
        return Err(unavailable());
    }
    let name = Path::new(crate::session_listener::PEERS_FILE);
    let file = harw_fsutil::open_beneath(dir.as_fd(), name, harw_fsutil::OpenMode::read_only())
        .map_err(|_| unavailable())?;
    harw_fsutil::ensure_private_regular(&file).map_err(|_| unavailable())?;
    let before = file.metadata().map_err(|_| unavailable())?;
    if before.nlink() != 1 || before.len() > POLICY_MAX_BYTES as u64 {
        return Err(unavailable());
    }
    let mut text = String::new();
    (&file).take(POLICY_MAX_BYTES as u64 + 1).read_to_string(&mut text)
        .map_err(|_| unavailable())?;
    let after = file.metadata().map_err(|_| unavailable())?;
    if text.len() as u64 != before.len() || after.len() != before.len()
        || after.nlink() != 1 || after.ctime() != before.ctime()
        || after.ctime_nsec() != before.ctime_nsec()
        || !harw_fsutil::same_file_at(dir.as_fd(), name, &file).map_err(|_| unavailable())?
    {
        return Err(unavailable());
    }
    validate_peer_pin_text(text).map_err(|_| unavailable())
}

fn validate_peer_pin_text(text: String) -> Result<String, String> {
    let (peers, skipped) = crate::session_listener::parse_peers(&text);
    let lines = text
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        })
        .count();
    if skipped != 0 || peers.len() != lines {
        return Err("remote peer pins are malformed or ambiguous".into());
    }
    Ok(text)
}

struct PeerPolicyGate {
    path: PathBuf,
    expected: Option<[u8; 32]>,
    fingerprints: std::collections::BTreeMap<String, [u8; 32]>,
    stale: AtomicBool,
}

impl PeerPolicyGate {
    fn open(config: &RemoteServeConfig) -> Result<Self, String> {
        let path = config
            .remote_dir()
            .join(crate::session_listener::PEERS_FILE);
        #[cfg(test)]
        if config.injected_peer_trust {
            return Ok(Self {
                path,
                expected: None,
                fingerprints: Default::default(),
                stale: AtomicBool::new(false),
            });
        }
        let text = read_peer_pins(&path)?;
        let digest = *blake3::hash(text.as_bytes()).as_bytes();
        if config.peer_pins_digest != Some(digest) {
            return Err("remote peer pins are not bound to the transport verifier".into());
        }
        Ok(Self {
            path,
            expected: Some(digest),
            fingerprints: crate::session_listener::peer_key_fingerprints(&text),
            stale: AtomicBool::new(false),
        })
    }

    fn current(&self) -> bool {
        self.current_with(|| read_peer_pins(&self.path))
    }

    fn current_with(&self, read: impl FnOnce() -> Result<String, String>) -> bool {
        if self.stale.load(Ordering::Acquire) {
            return false;
        }
        let Some(expected) = self.expected else {
            // Only an explicit cfg(test) fixture can omit disk trust.
            return true;
        };
        let current =
            read().is_ok_and(|text| *blake3::hash(text.as_bytes()).as_bytes() == expected);
        if !current {
            self.stale.store(true, Ordering::Release);
        }
        current && !self.stale.load(Ordering::Acquire)
    }
}

struct PolicyIdentityMapper {
    mapper: RegistryIdentityMapper,
    pins: Arc<PeerPolicyGate>,
    // The optional pinned Hub handshakes only for its fixed proof route;
    // a later registry edit must not make it a normal device connection.
    proof_only_hub: Option<NodeId>,
}

impl IdentityMapper for PolicyIdentityMapper {
    fn map(
        &self,
        peer: &harw_node_transport::AuthenticatedPeer,
        connection: harw_session_host::ConnectionId,
    ) -> harw_node_listener::MapFuture<'_> {
        if self.proof_only_hub.as_ref() == Some(&peer.node_id) || !self.pins.current() {
            return Box::pin(async { Err(harw_node_listener::ListenerError::UnknownPeer) });
        }
        let fingerprint = self.pins.fingerprints.get(peer.node_id.as_str()).copied();
        if self.pins.expected.is_some() && fingerprint.is_none() {
            return Box::pin(async { Err(harw_node_listener::ListenerError::UnknownPeer) });
        }
        let mapped = self.mapper.map(peer, connection);
        Box::pin(async move {
            let mut identity = mapped.await?;
            if !self.pins.current() {
                return Err(harw_node_listener::ListenerError::UnknownPeer);
            }
            identity.authenticated_key_fingerprint = fingerprint;
            Ok(identity)
        })
    }
}

/// A running remote ingress.
pub struct RemoteIngress {
    pub(crate) host: SessionHost,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    watcher: JoinHandle<()>,
    addr: SocketAddr,
    intake: harw_node_listener::StreamIntake,
    guards: Vec<Box<dyn std::any::Any + Send + Sync>>,
    #[cfg(all(unix, feature = "direct-owner-placement"))]
    pending_owner_reporter: Option<owner_placement::ConfiguredOwnerReporter>,
    #[cfg(all(unix, feature = "direct-owner-placement"))]
    owner_reporter: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for RemoteIngress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteIngress")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl RemoteIngress {
    /// The bound address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// A cloneable handle that feeds already-established byte streams
    /// (for example from an onion service) into the same node listener.
    #[must_use]
    pub(crate) fn intake(&self) -> harw_node_listener::StreamIntake {
        self.intake.clone()
    }

    /// Keeps `guard` alive until shutdown; dropping it stops that extra
    /// ingress, for example the onion service.
    pub(crate) fn attach(&mut self, guard: Box<dyn std::any::Any + Send + Sync>) {
        self.guards.push(guard);
    }

    /// Start reporting only after the configured owner inlet is installed.
    #[cfg(all(unix, feature = "direct-owner-placement"))]
    pub(crate) fn start_owner_reporter(&mut self) {
        if let Some(reporter) = self.pending_owner_reporter.take() {
            self.owner_reporter = Some(tokio::spawn(reporter.run(self.task.abort_handle(), self.shutdown.subscribe())));
        }
    }

    /// Stops accepting, closes live connections and waits for the listener.
    pub async fn shutdown(self) {
        let Self {
            shutdown,
            task,
            watcher,
            addr,
            guards,
            #[cfg(all(unix, feature = "direct-owner-placement"))]
            owner_reporter,
            ..
        } = self;
        let _ = shutdown.send(true);
        drop(guards);
        #[cfg(all(unix, feature = "direct-owner-placement"))]
        if let Some(reporter) = owner_reporter {
            let _ = reporter.await;
        }
        if let Err(error) = watcher.await {
            tracing::error!(%error, "remote revocation watcher failed");
        }
        if let Err(error) = task.await {
            tracing::error!(%error, "remote session ingress task failed");
        } else {
            tracing::info!(addr = %addr, "remote session ingress stopped");
        }
    }
}

/// Compose and start the remote ingress with the production driver over
/// `model`.
///
/// # Errors
/// A readable message when a directory, an adapter, the driver, the host or
/// the transport cannot be set up.
pub async fn start_remote(
    config: &RemoteServeConfig,
    model: Arc<dyn ModelProvider>,
    local: LocalNode,
    verifier: Arc<dyn NodeVerifier>,
    listener: TcpListener,
) -> Result<RemoteIngress, String> {
    if config.private_writer_readiness && (!config.private_writer_creation_receipts || !config.private_remote_writer) {
        return Err("private writer readiness requires the isolated writer and its creation receipts".into());
    }
    if config.private_writer_creation_receipts && !config.private_remote_writer {
        return Err("private writer creation receipts require the isolated private remote writer".into());
    }
    if config.private_remote_writer {
        if !matches!(config.share.mode, ShareMode::Off)
            || config.direct_owner_placement || config.direct_owner_status_instance.is_some()
            || config.cloud_keys.library.is_some() || config.cloud_keys.assistant.is_some()
            || config.provider_link_device.is_some()
        {
            return Err("private remote writer cannot share or expose placement, cloud custody, or provider linking".into());
        }
        #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
        writer_namespace::acquire(&config.remote_dir()).map_err(|error| error.to_string())?;
        #[cfg(not(all(target_os = "linux", feature = "direct-owner-placement")))]
        return Err("private remote writer unavailable in this build".into());
    }
    let share = open_share(config)?;
    let sessions_root = transcript_root(config, share.as_ref())?;
    let factory = GatewayCoreFactory::new(
        Arc::clone(&model),
        &sessions_root,
        config.home.clone(),
        config.cwd.clone(),
        0,
    )
    .with_tier(config.tier)
    .with_actor(config.actor());
    let factory = match &share {
        // The TUI's thread and store, and the session's own project folder.
        Some(shared) => factory
            .with_thread_mapper(
                crate::session_serve::cli_session_thread,
                &shared.sessions_root,
            )
            .with_session_cwd(workdir_resolver(shared.index.clone())),
        None => factory,
    };
    let driver =
        CoreTurnDriver::with_factory(CoreDriverConfig::new(sessions_root), Arc::new(factory))
            .and_then(|driver| driver.with_media_root(&config.home.join("media")))
            .map_err(|error| format!("remote session driver: {error}"))?;
    start_remote_composed(config, share, Arc::new(driver), Some(model), local, verifier, listener).await
}

struct GatewayAssistantModel(Arc<dyn ModelProvider>, Vec<String>);

/// The gateway operator explicitly admits model routes to this isolated room.
/// A device cannot select arbitrary configured credentials or provider URLs.
fn cloud_assistant_model_allowlist() -> Vec<String> {
    let Ok(raw) = std::env::var("HARW_CLOUD_ASSISTANT_MODELS") else { return Vec::new(); };
    if raw.len() > 32 * 1024 { return Vec::new(); }
    let mut routes: Vec<_> = raw.split(',').map(str::trim)
        .filter(|route| harw_protocol::session_wire::valid_cloud_assistant_model_route(route))
        .map(str::to_owned).collect();
    routes.sort_unstable();
    routes.dedup();
    routes.truncate(128);
    routes
}

impl harw_session_host::cloud_assistant::CloudAssistantModel for GatewayAssistantModel {
    fn answer<'a>(
        &'a self,
        turns: &'a [harw_protocol::session_wire::CloudAssistantTurn],
        text: &'a str,
    ) -> harw_session_host::driver::DriverFuture<'a, String> {
        self.answer_selected(turns, text, None)
    }

    fn model_choices(&self) -> Vec<String> { self.1.clone() }

    fn answer_selected<'a>(
        &'a self, turns: &'a [harw_protocol::session_wire::CloudAssistantTurn],
        text: &'a str, selected: Option<&'a str>,
    ) -> harw_session_host::driver::DriverFuture<'a, String> {
        use harw_core::{ConversationHistory, model::{ModelRequest, StopReason}};
        use harw_session_host::HostError;

        let mut history = ConversationHistory::new();
        // The scoped store already budgeted the union of selected and recent
        // pairs. Trimming again here would drop an explicitly recalled pair.
        for turn in turns {
            history.push_user_text(&turn.text);
            history.push_assistant_text(&turn.answer, None);
        }
        history.push_user_text(text);
        let request = ModelRequest::new(
            harw_extension_api::LoadedInstructions {
                system_prompt: "You are the Harw Cloud Assistant, not a coding agent. Reply in the user's language. You have no tools, file access, local device state or ability to act outside this conversation. Never claim to have performed an external action.".into(),
                fragments: Vec::new(),
            },
            Vec::new(),
            history,
            Vec::new(),
        ).with_max_output_tokens(Some(1024));
        Box::pin(async move {
            let mut request = request;
            if let Some(selected) = selected {
                if !self.1.iter().any(|route| route == selected) {
                    return Err(HostError::Denied("Selected Assistant model unavailable".into()));
                }
                let (provider, model) = selected.split_once('/').ok_or_else(|| HostError::Denied("Invalid Assistant model".into()))?;
                if self.0.pinned_provider_id().is_some_and(|id| id != provider)
                    || self.0.pinned_model_id().is_some_and(|id| id != model)
                {
                    return Err(HostError::Denied("Gateway model is pinned to a different route".into()));
                }
                request = request.with_provider_id(Some(harw_types::ProviderId::from(provider)))
                    .with_model_id(Some(harw_types::ModelId::from(model)));
            }
            let response = self.0.respond(request).await.map_err(|_| {
                HostError::Driver("Cloud Assistant model request failed".into())
            })?;
            if !response.tool_calls.is_empty() {
                return Err(HostError::Driver("Cloud Assistant does not execute tools".into()));
            }
            let stopped = match response.stop {
                StopReason::EndTurn | StopReason::StopSequence => None,
                StopReason::ContentFilter | StopReason::Refusal { .. } =>
                    Some("Cloud Assistant reply stopped by provider policy"),
                StopReason::MaxTokens => Some("Cloud Assistant reply reached the provider output limit"),
                StopReason::ContextWindowExceeded => Some("Cloud Assistant model context limit exceeded"),
                StopReason::StreamInterrupted => Some("Cloud Assistant provider stream interrupted"),
                _ => Some("Cloud Assistant model did not finish the reply"),
            };
            if let Some(reason) = stopped {
                return Err(HostError::Driver(reason.into()));
            }
            response.message.filter(|text| !text.trim().is_empty())
                .ok_or_else(|| HostError::Driver("Cloud Assistant model returned no text".into()))
        })
    }
}

/// Like [`start_remote`] around an injected `driver` (tests use a fake one).
///
/// # Errors
/// See [`start_remote`].
pub async fn start_remote_with_driver(
    config: &RemoteServeConfig,
    driver: Arc<dyn TurnDriver>,
    local: LocalNode,
    verifier: Arc<dyn NodeVerifier>,
    listener: TcpListener,
) -> Result<RemoteIngress, String> {
    if config.private_remote_writer || config.private_writer_creation_receipts || config.private_writer_readiness {
        return Err("private remote writer requires the production factory".into());
    }
    let share = open_share(config)?;
    start_remote_composed(config, share, driver, None, local, verifier, listener).await
}

/// Transcript and approval root: the profile store when sharing, otherwise
/// the remote ingress's private `<remote>/sessions` (created owner-only).
fn transcript_root(
    config: &RemoteServeConfig,
    share: Option<&SharedProfile>,
) -> Result<PathBuf, String> {
    if config.private_remote_writer {
        return private_writer_paths(config).map(|(_, sessions)| sessions);
    }
    match share {
        Some(shared) => Ok(shared.sessions_root.clone()),
        None => {
            let root = config.remote_dir().join(SESSIONS_DIR_NAME);
            private_dir(&root)?;
            Ok(root)
        }
    }
}

fn private_writer_paths(config: &RemoteServeConfig) -> Result<(PathBuf, PathBuf), String> {
    #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
    {
        writer_namespace::active_paths(&config.remote_dir()).map_err(|error| error.to_string())
    }
    #[cfg(not(all(target_os = "linux", feature = "direct-owner-placement")))]
    {
        let _ = config;
        Err("private remote writer unavailable in this build".into())
    }
}

/// Install each optional capability only after custody and profile scope are
/// validated. A bad key cannot turn a healthy gateway into an outage or cause
/// a fallback to plaintext, another service's key, or the local host.
fn mount_cloud_services(
    config: &RemoteServeConfig,
    host: &SessionHost,
    assistant_model: Option<Arc<dyn ModelProvider>>,
) {
    let CloudServiceKeys { library, assistant } = &config.cloud_keys;
    if library.is_none() && assistant.is_none() {
        return;
    }
    if library.as_ref().is_some_and(|path| Some(path) == assistant.as_ref()) {
        tracing::warn!("Cloud Library and Assistant unavailable: configure distinct credential files");
        return;
    }

    // The listener's state directory was selected by the gateway launcher.
    // If the active-profile selector moved since then, never combine the
    // old state directory with another profile's encryption context.
    let profile = harw_home::active_profile_name(&config.home);
    let valid_profile = harw_home::profile_dir(&config.home, &profile)
        .ok()
        .filter(|dir| dir.join(crate::session_serve::STATE_DIR_NAME) == config.state_dir);
    let Some(profile_dir) = valid_profile else {
        tracing::warn!("Cloud Library and Assistant unavailable: active profile does not match listener state");
        return;
    };

    if let Some(path) = library {
        if host.install_cloud_library(&profile_dir, path).is_ok() {
            tracing::info!("Cloud Library available to eligible enrolled remote devices");
        } else {
            tracing::warn!("Cloud Library unavailable: cannot open private store and credential");
        }
    }
    if let Some(path) = assistant {
        let Some(model) = assistant_model else {
            tracing::warn!("Cloud Assistant unavailable: no gateway model is configured");
            return;
        };
        let root = config.remote_dir().join("cloud-assistant-v1");
        let backend = private_dir(&root).ok().and_then(|()| {
            harw_session_host::cloud_assistant::CloudAssistant::open(
                root, path, profile, Arc::new(GatewayAssistantModel(model, cloud_assistant_model_allowlist())),
            ).ok()
        });
        if backend.is_some_and(|backend| host.install_cloud_assistant(Arc::new(backend)).is_ok()) {
            tracing::info!("Cloud Assistant available to eligible enrolled remote devices");
        } else {
            tracing::warn!("Cloud Assistant unavailable: cannot open private store and credential");
        }
    }
}

async fn start_remote_composed(
    config: &RemoteServeConfig,
    share: Option<SharedProfile>,
    driver: Arc<dyn TurnDriver>,
    assistant_model: Option<Arc<dyn ModelProvider>>,
    local: LocalNode,
    verifier: Arc<dyn NodeVerifier>,
    listener: TcpListener,
) -> Result<RemoteIngress, String> {
    let remote_dir = config.remote_dir();
    if config.direct_owner_placement && (config.direct_owner_status_instance.is_none()
        || !cfg!(all(unix, feature = "direct-owner-placement")))
    {
        return Err("fenced direct status requires an explicit owner instance and the direct-owner-placement feature".into());
    }
    let addr = listener
        .local_addr()
        .map_err(|error| format!("remote listener address: {error}"))?;
    let pins = Arc::new(PeerPolicyGate::open(config)?);
    #[cfg(all(unix, feature = "direct-owner-placement"))]
    let placement_authority = config
        .direct_owner_status_instance.as_ref()
        .filter(|_| config.direct_owner_placement)
        .map(|instance| owner_placement::ConfiguredPlacementAuthority::open(
            &remote_dir, local.clone(), Arc::clone(&pins), &instance.0, addr, config.owner_onion_ingress,
        ))
        .transpose()?;
    let (host_dir, sessions_root) = if config.private_remote_writer {
        if share.is_some() {
            return Err("private remote writer cannot share the profile store".into());
        }
        private_writer_paths(config)?
    } else {
        let sessions_root = transcript_root(config, share.as_ref())?;
        let host_dir = remote_dir.join("host");
        private_dir(&host_dir)?;
        (host_dir, sessions_root)
    };
    #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
    let creation_authority = config.private_writer_creation_receipts
        .then(|| writer_creation::open(&remote_dir, &local))
        .transpose()?;
    let transcripts = DurableTranscripts::open(&sessions_root)
        .map_err(|error| format!("remote transcripts: {error}"))?;
    // Only the host's own records: profile sessions may have live writers.
    let persisted_ids: Vec<_> = harw_session_host::record::RecordStore::new(&host_dir)
        .list()
        .map_err(|error| format!("remote hosted session records: {error}"))?
        .into_iter()
        .map(|record| record.session_id)
        .collect();
    transcripts
        .repair_tails(&persisted_ids)
        .map_err(|error| format!("remote transcript startup recovery: {error}"))?;
    let approvals = DurableApprovals::open(&sessions_root)
        .map_err(|error| format!("remote approvals: {error}"))?;
    let services = DaemonServices::new(driver, Arc::new(transcripts), Arc::new(approvals));
    let manage = share.is_some();
    let host = match share {
        None => SessionHost::open(
            HostConfig::new(host_dir),
            services.driver,
            services.transcripts,
            services.approvals,
        ),
        Some(shared) => SessionHost::open_with_profile(
            HostConfig::new(host_dir),
            services.driver,
            services.transcripts,
            services.approvals,
            ToolHost::disabled(),
            ProfileShare {
                settings: shared.settings,
                index: shared.index,
            },
        ),
    };
    let host = Arc::new(host.map_err(|error| format!("remote host: {error}"))?);
    #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
    if let Some((authority, namespace)) = creation_authority {
        let remote = remote_dir.clone();
        let guarded = Arc::new(move || writer_namespace::active_namespace_id(&remote).ok() == Some(namespace));
        host.install_owner_creation_authority(authority, guarded)
            .map_err(|error| format!("private writer creation authority: {error}"))?;
    }
    // Both services remain exclusive to this authenticated node ingress.
    mount_cloud_services(config, &host, assistant_model);
    // Management ops only with a shared profile (contract K-W3c WR-7).
    if manage {
        host.install_ops(Arc::new(crate::session_ops::HostedOps::open(config.home.clone())?))
            .map_err(|error| format!("remote ops: {error}"))?;
        // Global jobs and agents views over the profile's durable job store and
        // agent library (feature `overview`); tenants are checked by the host.
        let profile = harw_home::active_profile_name(&config.home);
        let profile_dir = harw_home::profile_dir(&config.home, &profile)
            .map_err(|error| format!("remote overview: profile directory: {error}"))?;
        let layers = vec![config.home.clone(), profile_dir.clone()];
        let library: harw_session_host::overview::AgentLibrary =
            Arc::new(move || harw_local_host::agent_library(&layers));
        host.install_overview(Arc::new(harw_session_host::overview::Overview::new(
            Some(Arc::new(harw_session_store::JobStore::new(&profile_dir))),
            Some(library),
        )))
        .map_err(|error| format!("remote overview: {error}"))?;
    }
    #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
    let readiness = config.private_writer_readiness.then(|| {
        writer_readiness_lane::prepare(host.as_ref().clone(), &remote_dir, &local)
    }).transpose()?;
    #[cfg(all(unix, feature = "direct-owner-placement"))]
    let proof_hub = placement_authority.as_ref().map(|(authority, _)| authority.hub_node().clone());
    #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
    let proof_hub = proof_hub.or_else(|| readiness.as_ref().map(|lane| lane.hub().clone()));
    let authenticated_local_node = local.node_id().clone();
    let server = NodeTransportServer::new(local, verifier, ServerOptions::default())
        .map_err(|error| format!("node transport: {error}"))?;
    // Validate bounded owner policy before accepting authenticated connections.
    let devices = DeviceRegistry::new(&remote_dir)
        .scan_bounded(POLICY_MAX_BYTES)
        .map_err(|error| format!("remote device policy: {error}"))?;
    #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
    if let Some(lane) = &readiness
        && (!devices.rejected.is_empty() || devices.records.iter().any(|row| &row.node_id == lane.hub()))
    {
        return Err("the proof-only Hub must not appear in the session device registry".into());
    }
    if config.direct_owner_status_instance.is_some()
        && (pins.expected.is_none() || !pins.current() || !devices.rejected.is_empty()
            || !devices.records.iter().any(|row| {
                !row.revoked && pins.fingerprints.get(row.node_id.as_str())
                    .is_some_and(|fingerprint| *fingerprint != [0; 32])
            }))
    {
        return Err("direct owner status requires verified peer pins and an active enrolled device".into());
    }
    // Current membership is host-internal metadata, not a new admission path.
    // Bind the authority to the exact mounted ingress, not a mutable active selector.
    match crate::admin::membership::Registry::open(remote_dir.clone()) {
        Ok(resolver) => host.install_user_memberships(Arc::new(resolver))
            .map_err(|error| format!("remote membership composition: {error}"))?,
        Err(_) if config.direct_owner_status_instance.is_some() => {
            return Err("direct owner status requires the current user membership authority".into());
        }
        Err(_) => tracing::warn!("Current user membership lookup unavailable"),
    }
    let mapper: Arc<dyn IdentityMapper> = Arc::new(PolicyIdentityMapper {
        mapper: RegistryIdentityMapper::new(&remote_dir),
        pins: Arc::clone(&pins),
        #[cfg(all(unix, feature = "direct-owner-placement"))]
        proof_only_hub: proof_hub,
        #[cfg(not(all(unix, feature = "direct-owner-placement")))]
        proof_only_hub: None,
    });
    let node_listener = NodeListener::new(server, Arc::clone(&host), mapper, WsLimits::default());
    #[cfg(all(unix, feature = "direct-owner-placement"))]
    let mut pending_owner_reporter = None;
    let node_listener = if let Some(instance) = &config.direct_owner_status_instance {
        // This is the exact newly opened host, not a configured epoch or a
        // remote report. OwnerProcessPin resolves this host's native epoch.
        let pin = Arc::new(host.pin_owner_process(authenticated_local_node.as_str(), &instance.0)
            .map_err(|_| "direct owner status could not bind the local SessionHost".to_owned())?);
        #[cfg(all(unix, feature = "direct-owner-placement"))]
        let node_listener = if let Some((authority, custody)) = placement_authority {
            if devices.records.iter().any(|row| !row.revoked && &row.node_id == authority.hub_node()) {
                return Err("the pinned Root-Hub node must not be enrolled as an owner device".into());
            }
            let proof = authority.compose_proof(&host, custody)?;
            let selected_ingress = authority.ingress();
            pending_owner_reporter = authority.reporter(Arc::clone(&pin));
            host.install_direct_owner_placement_authority(Arc::new(authority))
                .map_err(|_| "could not mount the pinned direct owner placement authority".to_owned())?;
            node_listener.with_owner_observe_proof(proof, selected_ingress)
        } else {
            node_listener
        };
        node_listener.with_direct_owner_status(pin)
    } else {
        node_listener
    };
    let node_listener = match &config.provider_link_device {
        Some(owner) => node_listener.with_provider_link(Arc::new(AccountLink {
            owner: owner.clone(),
            home: config.home.clone(),
        })),
        None => node_listener,
    };
    let intake = node_listener.intake();
    let revoker = node_listener.revoker().with_registry(&remote_dir);
    let (shutdown, mut rx) = watch::channel(false);
    let watcher = tokio::spawn(watch_revocations(
        revoker,
        remote_dir.clone(),
        config.revocation_poll,
        shutdown.subscribe(),
        pins,
    ));
    let task = tokio::spawn(async move {
        let stop = async move {
            let _ = rx.wait_for(|stop| *stop).await;
        };
        if let Err(error) = node_listener.serve(listener, stop).await {
            tracing::warn!(%error, "remote session ingress ended with an error");
        }
    });
    let mut guards: Vec<Box<dyn std::any::Any + Send + Sync>> = Vec::new();
    #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
    if let Some(lane) = readiness {
        guards.push(Box::new(lane.start(task.abort_handle(), shutdown.subscribe())));
    }
    Ok(RemoteIngress {
        host: host.as_ref().clone(),
        shutdown,
        task,
        watcher,
        addr,
        intake,
        guards,
        #[cfg(all(unix, feature = "direct-owner-placement"))]
        pending_owner_reporter,
        #[cfg(all(unix, feature = "direct-owner-placement"))]
        owner_reporter: None,
    })
}

/// Revalidate owner policy on a bounded poll. Policy edits cut old connection
/// authority, including detached creators; fresh authentication uses the new
/// registry. Explicit revocation remains permanent. Immutable pin changes latch
/// closed until restart, including for future mappings.
async fn watch_revocations(
    revoker: HostRevoker,
    registry_dir: PathBuf,
    every: Duration,
    mut stop: watch::Receiver<bool>,
    pins: Arc<PeerPolicyGate>,
) {
    let registry = DeviceRegistry::new(&registry_dir);
    let mut applied: HashSet<DeviceId> = HashSet::new();
    let mut ticker = tokio::time::interval(every.max(Duration::from_millis(10)));
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.wait_for(|stop| *stop) => return,
        }
        let identities = revoker.authenticated_identities();
        let scan = registry.scan_bounded(POLICY_MAX_BYTES);
        // Extract explicit revocations even if another row is malformed.
        if let Ok(scan) = &scan {
            for record in scan.records.iter().filter(|record| record.revoked) {
                if applied.insert(record.device.clone())
                    && let Err(error) = revoker.revoke_device(&record.device)
                {
                    tracing::warn!(%error, "device revocation incomplete");
                }
            }
        }
        let records = match scan {
            Ok(scan) if scan.rejected.is_empty() && pins.current() => scan.records,
            _ => {
                for identity in &identities {
                    if let Some(device) = &identity.device {
                        revoker.invalidate_device_policy(device);
                    }
                }
                tracing::warn!(
                    "remote policy unavailable or changed pins; cached authority invalidated"
                );
                continue;
            }
        };
        let invalid = invalidated_devices(&records, &identities);
        for device in invalid {
            revoker.invalidate_device_policy(&device);
        }
    }
}

fn invalidated_devices(
    records: &[DeviceRecord],
    identities: &[harw_session_host::ClientIdentity],
) -> HashSet<DeviceId> {
    let mut invalid = HashSet::new();
    for identity in identities {
        let Some(device) = &identity.device else {
            continue;
        };
        let mut matching = records.iter().filter(|record| &record.device == device);
        let valid = matching.next().is_some_and(|record| {
            record.authorizes_identity(identity)
                && records
                    .iter()
                    .filter(|other| other.node_id == record.node_id)
                    .count()
                    == 1
        }) && matching.next().is_none();
        if !valid {
            invalid.insert(device.clone());
        }
    }
    invalid
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{KeyPair as _, ML_DSA_65_SIGNING, PqdsaKeyPair};
    use harw_node_listener::{DeviceRecord, DeviceRegistry};
    use harw_node_transport::{
        ML_DSA_65_SIGNATURE_LEN, NodeIdentity, NodeSigner, PinnedPeers, SignFuture,
        TranscriptWrappedSigner, TranscriptWrappedVerifier,
    };
    use harw_protocol::SessionPort;
    use harw_session_host::driver::{
        CancelSignal, DriverFuture, EventSink, Setting, TurnInput, TurnOutcome,
    };
    use harw_session_remote::{ConnectOptions, NodeEndpoint, RemotePort, connect_node};
    use harw_types::{NodeId, SessionId, TenantId};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn injected_config(path: &Path) -> RemoteServeConfig {
        let mut config = RemoteServeConfig::new(path, path, path);
        config.injected_peer_trust = true;
        config
    }

    struct SeedSigner(PqdsaKeyPair);

    impl NodeSigner for SeedSigner {
        fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
            let mut signature = vec![0u8; ML_DSA_65_SIGNATURE_LEN];
            let result = self
                .0
                .sign(transcript, &mut signature)
                .map(|n| {
                    signature.truncate(n);
                    signature
                })
                .map_err(|_| harw_node_transport::SignerError::new("test signer failed"));
            Box::pin(std::future::ready(result))
        }
    }

    struct TestNode {
        id: NodeId,
        identity: NodeIdentity,
        local: LocalNode,
    }

    fn node(name: &str, seed: u8) -> TestResult<TestNode> {
        let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32]).map_err(|_| "seed")?;
        let id = NodeId::try_from_str(name)?;
        let identity = NodeIdentity {
            node_id: id.clone(),
            public_key: key.public_key().as_ref().to_vec(),
            key_ref: format!("test-only/{name}"),
        };
        let local = LocalNode::new(identity.clone(), Arc::new(SeedSigner(key)));
        Ok(TestNode {
            id,
            identity,
            local,
        })
    }

    struct NoopDriver;

    impl TurnDriver for NoopDriver {
        // This test double performs no privileged runtime/tool effects.
        fn prepare_execution(
            &self,
            _: &harw_types::SessionId,
            _: harw_session_host::driver::ExecutionAdmission,
        ) -> harw_session_host::driver::DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn run_turn(
            &self,
            _: TurnInput,
            _: CancelSignal,
            _: Arc<dyn EventSink>,
        ) -> DriverFuture<'_, TurnOutcome> {
            Box::pin(async { Ok(TurnOutcome::Completed) })
        }
        fn resume_after_approval(
            &self,
            _: &SessionId,
            _: CancelSignal,
            _: Arc<dyn EventSink>,
        ) -> DriverFuture<'_, TurnOutcome> {
            Box::pin(async { Ok(TurnOutcome::Completed) })
        }
        fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn model_name(&self, _: &SessionId) -> Option<String> {
            None
        }
    }

    fn pinned(identity: &NodeIdentity) -> TestResult<PinnedPeers> {
        let mut peers = PinnedPeers::new();
        peers.pin_identity(identity)?;
        Ok(peers)
    }

    async fn dial(
        addr: SocketAddr,
        gateway: &TestNode,
        phone: &TestNode,
    ) -> Result<RemotePort, harw_session_remote::RemoteError> {
        let verifier = pinned(&gateway.identity)
            .map_err(|error| harw_session_remote::RemoteError::Connect(error.to_string()))?;
        let connection = connect_node(
            NodeEndpoint {
                addr,
                expected_node: gateway.id.clone(),
            },
            &phone.local,
            &verifier,
            ConnectOptions::new("phone"),
        )
        .await?;
        Ok(RemotePort::new(connection))
    }

    #[test]
    fn defaults_are_conservative_and_the_actor_is_a_placeholder_without_an_approver() -> TestResult
    {
        let config = RemoteServeConfig::new(Path::new("/s"), Path::new("/h"), Path::new("/w"));
        assert_eq!(config.tier, PermissionTier::Observer);
        assert_eq!(
            config.actor(),
            ApprovalActor::Operator {
                id: NO_APPROVER.to_owned()
            }
        );
        let mut with = config.clone();
        with.approval_device = Some(DeviceId::try_from_str("dev-1")?);
        assert_eq!(
            with.actor(),
            ApprovalActor::Operator {
                id: "device:dev-1".to_owned()
            }
        );
        Ok(())
    }

    #[cfg(unix)]
    fn fixture_key(path: &Path, data: &[u8]) -> TestResult {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .open(path)?.write_all(data)?;
        Ok(())
    }

    #[cfg(unix)]
    fn provision_cloud_fixture(
        home: &Path, phone: &TestNode, tenant: &TenantId, library_bytes: &[u8],
    ) -> TestResult<RemoteServeConfig> {
        let profile = harw_home::profile_dir(home, &harw_home::active_profile_name(home))?;
        let mut config = RemoteServeConfig::new(
            &profile.join(crate::session_serve::STATE_DIR_NAME), home, home,
        );
        config.cloud_keys = CloudServiceKeys {
            library: Some(home.join("library-fixture.key")),
            assistant: Some(home.join("assistant-fixture.key")),
        };
        fixture_key(&home.join("library-fixture.key"), library_bytes)?;
        fixture_key(&home.join("assistant-fixture.key"), &[29; 32])?;
        private_dir(&config.remote_dir())?;
        enroll(&config, phone, tenant)?;
        let key: String = phone.identity.public_key.iter().map(|byte| format!("{byte:02x}")).collect();
        let pins = format!("{}|{key}\n", phone.id.as_str());
        std::fs::write(config.remote_dir().join(crate::session_listener::PEERS_FILE), &pins)?;
        config.peer_pins_digest = Some(*blake3::hash(pins.as_bytes()).as_bytes());
        Ok(config)
    }

    #[cfg(unix)]
    fn cloud_fixture_options() -> ConnectOptions {
        use harw_protocol::session_wire::features;
        let mut options = ConnectOptions::new("cloud-service-fixture");
        options.features = vec![features::CLOUD_LIBRARY_V1.into(), features::CLOUD_ASSISTANT_V1.into()];
        options
    }

    #[cfg(unix)]
    async fn dial_cloud_fixture(
        addr: SocketAddr, gateway: &TestNode, phone: &TestNode,
    ) -> TestResult<RemotePort> {
        let connection = connect_node(
            NodeEndpoint { addr, expected_node: gateway.id.clone() },
            &phone.local,
            &pinned(&gateway.identity)?,
            cloud_fixture_options(),
        ).await?;
        Ok(RemotePort::new(connection))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cloud_services_mount_on_enrolled_node_only_and_reopen_committed_data() -> TestResult {
        use crate::session_serve::{SessionServeConfig, start_shared};
        use harw_protocol::cloud_library::{CloudLibraryCommandV1, CloudLibraryReplyV1};
        use harw_protocol::session_wire::{CloudAssistantStatus, CloudAssistantSubmit, features};
        let home = tempfile::tempdir()?;
        let gateway = node("cloud-fixture-gateway", 1)?;
        let phone = node("cloud-fixture-client", 2)?;
        let tenant = TenantId::try_from_str("cloud-fixture")?;
        let config = provision_cloud_fixture(home.path(), &phone, &tenant, &[17; 32])?;
        let model = harw_core::RecordingModelProvider::with_response("fixture answer");
        let start = || async {
            start_remote_composed(
                &config, None, Arc::new(NoopDriver), Some(Arc::new(model.clone())),
                gateway.local.clone(), Arc::new(pinned(&phone.identity)?),
                TcpListener::bind("127.0.0.1:0").await?,
            ).await.map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { error.into() })
        };
        let ingress = start().await?;
        let peer = dial_cloud_fixture(ingress.addr(), &gateway, &phone).await?;
        let flags = &peer.connection().hello_ack().features;
        assert!(flags.iter().any(|flag| flag == features::CLOUD_LIBRARY_V1));
        assert!(flags.iter().any(|flag| flag == features::CLOUD_ASSISTANT_V1));

        let unix = start_shared(
            &SessionServeConfig::for_profile(home.path(), home.path(), home.path(), None, None),
            ingress.host.clone(), tenant.clone(),
        ).await?;
        let desktop = RemotePort::new(harw_session_remote::connect_unix(
            unix.socket().to_owned(), cloud_fixture_options(),
        ).await?);
        assert!(desktop.connection().hello_ack().features.is_empty());
        assert!(desktop.cloud_assistant_status().await.is_err());
        assert!(desktop.cloud_library(CloudLibraryCommandV1::List).await.is_err());
        drop(desktop);

        let count = peer.cloud_library(CloudLibraryCommandV1::List).await?;
        assert!(matches!(count, CloudLibraryReplyV1::Snapshot { revision: 0, assets } if assets.is_empty()));
        let imported = peer.cloud_library(CloudLibraryCommandV1::Import {
            expected_revision: 0, name: "fixture inset".into(),
            data_b64: "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/2wBDAQkJCQwLDBgNDRgyIRwhMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjL/wAARCAABAAEDASIAAhEBAxEB/8QAFAABAAAAAAAAAAAAAAAAAAAAAP/EABQQAQAAAAAAAAAAAAAAAAAAAAD/xAAUAQEAAAAAAAAAAAAAAAAAAAAA/8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAwDAQACEQMRAD8AAA//2Q==".into(),
        }).await?;
        assert!(matches!(imported, CloudLibraryReplyV1::Updated { revision: 1 }));
        assert_eq!(peer.cloud_assistant_status().await?.status, CloudAssistantStatus::Unprovisioned);
        assert_eq!(peer.cloud_assistant_provision().await?.status, CloudAssistantStatus::Ready);
        let submit = || CloudAssistantSubmit { client_message_id: "fixture-message".into(), text: "fixture question".into() };
        assert_eq!(peer.cloud_assistant_submit(submit()).await?.turns[0].answer, "fixture answer");
        assert_eq!(model.recorded().len(), 1);
        let request = model.last().ok_or("missing provider request")?;
        assert!(request.tools.is_empty() && request.context.is_empty() && request.identity.is_none());
        drop(peer);
        unix.shutdown().await;
        ingress.shutdown().await;

        let ingress = start().await?;
        let peer = dial_cloud_fixture(ingress.addr(), &gateway, &phone).await?;
        let count = peer.cloud_library(CloudLibraryCommandV1::List).await?;
        assert!(matches!(count, CloudLibraryReplyV1::Snapshot { revision: 1, assets } if assets.len() == 1 && assets[0].name == "fixture inset"));
        assert_eq!(peer.cloud_assistant_submit(submit()).await?.turns[0].answer, "fixture answer");
        assert_eq!(model.recorded().len(), 1, "retry must use the committed reply");
        drop(peer);
        ingress.shutdown().await;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_library_key_leaves_other_gateway_capabilities_available() -> TestResult {
        use harw_protocol::session_wire::features;
        let home = tempfile::tempdir()?;
        let gateway = node("cloud-bad-key-gateway", 3)?;
        let phone = node("cloud-bad-key-client", 4)?;
        let tenant = TenantId::try_from_str("cloud-fixture")?;
        let config = provision_cloud_fixture(home.path(), &phone, &tenant, &[19; 31])?;
        let ingress = start_remote_composed(
            &config, None, Arc::new(NoopDriver), Some(Arc::new(harw_core::RecordingModelProvider::new())),
            gateway.local.clone(), Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        ).await?;
        let peer = dial_cloud_fixture(ingress.addr(), &gateway, &phone).await?;
        let flags = &peer.connection().hello_ack().features;
        assert!(flags.iter().any(|flag| flag == features::CLOUD_ASSISTANT_V1));
        assert!(!flags.iter().any(|flag| flag == features::CLOUD_LIBRARY_V1));
        assert!(peer.list().await?.is_empty());
        drop(peer);
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn remote_start_repairs_torn_transcript_for_its_host_record() -> TestResult {
        use harw_session_host::record::{HostedSessionRecord, RecordStore};
        use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
        use harw_types::ThreadRef;
        use std::io::Write as _;

        let temp = tempfile::tempdir()?;
        let config = injected_config(temp.path());
        let remote_dir = config.remote_dir();
        let id = SessionId::try_from_str("repair-remote")?;
        let now = jiff::Timestamp::now();
        RecordStore::new(&remote_dir.join("host")).save(&HostedSessionRecord {
            session_id: id.clone(),
            tenant: None,
            owner_principal: "operator".to_owned(),
            fresh_human_origin: false,
            creator_execution: None,
            manual_cloud_work_receipt: None,
            exact_rewrites: Vec::new(),
            workspace: None,
            title: None,
            created_at: now,
            updated_at: now,
            state: harw_protocol::HostedState::Idle,
            host_epoch: 0,
            generation: 0,
            owner_agent: None,
            tool_agents: Vec::new(),
        })?;
        let sessions = remote_dir.join("sessions");
        std::fs::create_dir_all(&sessions)?;
        let store = TranscriptStore::new(&sessions);
        store.append(&TranscriptRecord::new(
            id.clone(),
            ThreadRef::from_str("root"),
            0,
            now,
            RecordKind::Item,
            serde_json::json!({"type":"user_message","id":"i","content":[]}),
        ))?;
        let path = store.transcript_path(&id)?;
        let valid = std::fs::read(&path)?;
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        file.write_all(b"{\"torn\"")?;
        drop(file);
        let gateway = node("gateway-repair", 1)?;
        let phone = node("phone-repair", 2)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local,
            Arc::new(pinned(&phone.identity)?),
            listener,
        )
        .await?;
        assert_eq!(std::fs::read(&path)?, valid);
        assert!(
            std::fs::read_dir(&sessions)?.any(|entry| entry.ok().is_some_and(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("repair-remote.jsonl.corrupt-")))
        );
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn phone_and_unix_client_share_sessions_in_the_selected_tenant() -> TestResult {
        use crate::session_serve::{SessionServeConfig, start_shared};
        use harw_protocol::session_wire::CreateParams;
        use harw_session_remote::connect_unix;
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let config = injected_config(dir);
        let gateway = node("gateway", 1)?;
        let phone = node("phone", 2)?;
        let tenant = TenantId::try_from_str("shared")?;
        std::fs::create_dir_all(config.remote_dir())?;
        DeviceRegistry::new(&config.remote_dir()).enroll(&DeviceRecord {
            node_id: phone.id.clone(),
            device: DeviceId::try_from_str("phone")?,
            tenant: tenant.clone(),
            tier: PermissionTier::Operator,
            revoked: false,
            label: "phone".into(),
            approve_optin: false,
        })?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        let local_config = SessionServeConfig::for_profile(dir, dir, dir, None, None);
        let unix = start_shared(&local_config, ingress.host.clone(), tenant.clone()).await?;
        let desktop = RemotePort::new(
            connect_unix(unix.socket().to_owned(), ConnectOptions::new("desktop")).await?,
        );
        let mobile = dial(ingress.addr(), &gateway, &phone).await?;
        let session = desktop
            .create(CreateParams {
                title: Some("desktop".into()),
                workspace: None,
                fork_from: None,
            })
            .await?;
        assert_eq!(session.tenant.as_ref(), Some(&tenant));
        assert!(
            mobile
                .list()
                .await?
                .iter()
                .any(|s| s.session_id == session.session_id)
        );
        let second = mobile
            .create(CreateParams {
                title: Some("phone".into()),
                workspace: None,
                fork_from: None,
            })
            .await?;
        assert!(
            desktop
                .list()
                .await?
                .iter()
                .any(|s| s.session_id == second.session_id)
        );
        assert_eq!(ingress.host.listener_enabled("local"), Some(true));
        assert_eq!(ingress.host.listener_enabled("node"), Some(true));
        // Disabling node ingress stops new handshakes. The local recovery path remains available.
        ingress
            .host
            .register_listener(harw_protocol::session_wire::GatewayListenerInfo {
                name: "node".into(),
                kind: harw_protocol::session_wire::ListenerKind::Node,
                address: ingress.addr().to_string(),
                enabled: false,
            });
        assert!(dial(ingress.addr(), &gateway, &phone).await.is_err());
        let local_reconnect =
            connect_unix(unix.socket().to_owned(), ConnectOptions::new("desktop")).await?;
        drop(local_reconnect);
        drop((desktop, mobile));
        unix.shutdown().await;
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn only_designated_enrolled_device_can_link_the_account() -> TestResult {
        use harw_node_transport::{NodeTransportClient, full_body};
        let temp = tempfile::tempdir()?;
        let mut config = injected_config(temp.path());
        let gateway = node("gateway", 1)?;
        let owner = node("owner-phone", 2)?;
        let guest = node("other-phone", 3)?;
        let mut peers = pinned(&owner.identity)?;
        peers.pin_identity(&guest.identity)?;
        config.provider_link_device = Some(DeviceId::try_from_str("owner-phone")?);
        let remote_dir = config.remote_dir();
        std::fs::create_dir_all(&remote_dir)?;
        for phone in [&owner, &guest] {
            DeviceRegistry::new(&remote_dir).enroll(&DeviceRecord {
                node_id: phone.id.clone(),
                device: DeviceId::try_from_str(phone.id.as_str())?,
                tenant: TenantId::try_from_str("tenant")?,
                tier: PermissionTier::Operator,
                revoked: false,
                label: "phone".into(),
                approve_optin: false,
            })?;
        }
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(peers),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        for (phone, expected) in [
            (&guest, http::StatusCode::FORBIDDEN),
            (&owner, http::StatusCode::BAD_REQUEST),
        ] {
            let mut client = NodeTransportClient::connect(
                ingress.addr(),
                &phone.local,
                &gateway.id,
                &pinned(&gateway.identity)?,
            )
            .await?;
            let request = http::Request::post("/v1/provider-link/chatgpt")
                .header("host", "gateway")
                .body(full_body("invalid"))?;
            assert_eq!(client.send_request(request).await?.status(), expected);
        }
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_enrolled_device_lists_sessions_over_the_node_transport_and_revocation_cuts_it()
    -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = injected_config(temp.path());
        config.revocation_poll = Duration::from_millis(50);
        let gateway = node("gateway", 1)?;
        let phone = node("phone", 2)?;
        let device = DeviceId::try_from_str("dev-1")?;
        // The registry lives in the remote directory; enrol the phone.
        let remote_dir = config.remote_dir();
        std::fs::create_dir_all(&remote_dir)?;
        DeviceRegistry::new(&remote_dir).enroll(&DeviceRecord {
            node_id: phone.id.clone(),
            device: device.clone(),
            tenant: TenantId::try_from_str("tenant-a")?,
            tier: PermissionTier::Operator,
            revoked: false,
            label: "phone".to_owned(),
            approve_optin: false,
        })?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            listener,
        )
        .await?;

        let port = dial(ingress.addr(), &gateway, &phone).await?;
        assert!(port.list().await?.is_empty());

        // Live revocation by the operator's file edit: the open connection is
        // cut and the next handshake is refused.
        DeviceRegistry::new(&remote_dir).mark_revoked(&device)?;
        let mut cut = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if port.list().await.is_err() {
                cut = true;
                break;
            }
        }
        assert!(cut, "the open connection of a revoked device is closed");
        drop(port);
        assert!(
            dial(ingress.addr(), &gateway, &phone).await.is_err(),
            "a revoked device must not connect again"
        );
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn live_policy_edits_cut_old_authority_and_valid_registry_recovery_reconnects()
    -> TestResult {
        for change in [
            "tier",
            "tenant",
            "approval",
            "removed",
            "ambiguous-device",
            "ambiguous-node",
            "unreadable",
            "malformed",
            "node",
        ] {
            let temp = tempfile::tempdir()?;
            let mut config = injected_config(temp.path());
            config.revocation_poll = Duration::from_millis(10);
            let gateway = node("gateway-policy", 1)?;
            let phone = node("phone-policy", 2)?;
            let remote_dir = config.remote_dir();
            std::fs::create_dir_all(&remote_dir)?;
            let registry = DeviceRegistry::new(&remote_dir);
            let original = DeviceRecord {
                node_id: phone.id.clone(),
                device: DeviceId::try_from_str("dev-policy")?,
                tenant: TenantId::try_from_str("tenant-a")?,
                tier: PermissionTier::Owner,
                revoked: false,
                label: "phone".into(),
                approve_optin: true,
            };
            registry.enroll(&original)?;
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let ingress = start_remote_with_driver(
                &config,
                Arc::new(NoopDriver),
                gateway.local.clone(),
                Arc::new(pinned(&phone.identity)?),
                listener,
            )
            .await?;
            let port = dial(ingress.addr(), &gateway, &phone).await?;
            assert!(port.list().await?.is_empty());
            let path = remote_dir.join("node-devices.conf");
            let mut changed = original.clone();
            match change {
                "tier" => changed.tier = PermissionTier::Observer,
                "tenant" => changed.tenant = TenantId::try_from_str("tenant-b")?,
                "approval" => changed.approve_optin = false,
                "node" => changed.node_id = NodeId::try_from_str("replacement-node")?,
                _ => {}
            }
            match change {
                "removed" => std::fs::remove_file(&path)?,
                "ambiguous-device" => {
                    changed.node_id = NodeId::try_from_str("another-node")?;
                    registry.enroll(&changed)?;
                }
                "ambiguous-node" => {
                    changed.device = DeviceId::try_from_str("another-device")?;
                    registry.enroll(&changed)?;
                }
                "unreadable" => {
                    std::fs::remove_file(&path)?;
                    std::fs::create_dir(&path)?;
                }
                "malformed" => {
                    std::fs::write(&path, format!("{}\nmalformed-row\n", original.render()))?
                }
                _ => std::fs::write(&path, format!("{}\n", changed.render()))?,
            }
            let cut = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if port.list().await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
            assert!(cut.is_ok(), "stale authority survived {change}");
            drop(port);
            // Errors/removal are temporary policy invalidation, never a device ban.
            if path.is_dir() {
                std::fs::remove_dir(&path)?;
            }
            let mut recovered = original.clone();
            recovered.tier = PermissionTier::Observer;
            recovered.approve_optin = false;
            std::fs::write(&path, format!("{}\n", recovered.render()))?;
            let fresh = dial(ingress.addr(), &gateway, &phone).await?;
            assert!(
                fresh.list().await?.is_empty(),
                "fresh recovery failed after {change}"
            );
            ingress.shutdown().await;
        }
        Ok(())
    }

    #[tokio::test]
    async fn pin_removal_cuts_live_authority_and_rejects_reconnect_until_restart() -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = RemoteServeConfig::new(temp.path(), temp.path(), temp.path());
        config.revocation_poll = Duration::from_millis(10);
        let remote_dir = config.remote_dir();
        std::fs::create_dir_all(&remote_dir)?;
        let gateway = node("gateway-pins", 1)?;
        let phone = node("phone-pins", 2)?;
        DeviceRegistry::new(&remote_dir).enroll(&DeviceRecord {
            node_id: phone.id.clone(),
            device: DeviceId::try_from_str("dev-pins")?,
            tenant: TenantId::try_from_str("tenant-a")?,
            tier: PermissionTier::Observer,
            revoked: false,
            label: "phone".into(),
            approve_optin: false,
        })?;
        let key: String = phone
            .identity
            .public_key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let text = format!("{}|{key}\n", phone.id.as_str());
        let path = remote_dir.join(crate::session_listener::PEERS_FILE);
        std::fs::write(&path, &text)?;
        config.peer_pins_digest = Some(*blake3::hash(text.as_bytes()).as_bytes());
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            listener,
        )
        .await?;
        let port = dial(ingress.addr(), &gateway, &phone).await?;
        assert!(port.list().await?.is_empty());
        std::fs::remove_file(&path)?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if port.list().await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        std::fs::write(&path, &text)?;
        assert!(dial(ingress.addr(), &gateway, &phone).await.is_err());
        ingress.shutdown().await;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let restarted = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            listener,
        )
        .await?;
        assert!(
            dial(restarted.addr(), &gateway, &phone)
                .await?
                .list()
                .await?
                .is_empty()
        );
        restarted.shutdown().await;
        Ok(())
    }

    #[test]
    fn owner_pin_gate_requires_exact_bounded_unambiguous_file_and_latches_until_restart()
    -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = RemoteServeConfig::new(temp.path(), temp.path(), temp.path());
        std::fs::create_dir_all(config.remote_dir())?;
        let path = config
            .remote_dir()
            .join(crate::session_listener::PEERS_FILE);
        assert!(
            PeerPolicyGate::open(&config).is_err(),
            "missing pins must fail closed"
        );
        let phone = node("phone-pin", 2)?;
        let key: String = phone
            .identity
            .public_key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let text = format!("{}|{key}\n", phone.id.as_str());
        std::fs::write(&path, &text)?;
        assert!(
            PeerPolicyGate::open(&config).is_err(),
            "unbound verifier must fail closed"
        );
        config.peer_pins_digest = Some(*blake3::hash(text.as_bytes()).as_bytes());
        let gate = PeerPolicyGate::open(&config)?;
        assert!(gate.current());
        let concurrent = PeerPolicyGate::open(&config)?;
        assert!(!concurrent.current_with(|| {
            // Another lookup detects a change while this lookup is reading.
            concurrent.stale.store(true, Ordering::Release);
            Ok(text.clone())
        }));
        std::fs::write(&path, format!("{text}{text}"))?;
        assert!(
            read_peer_pins(&path).is_err(),
            "duplicate node pins are ambiguous"
        );
        assert!(!gate.current());
        std::fs::write(&path, &text)?;
        assert!(!gate.current(), "restoring static trust requires a restart");
        let restarted = PeerPolicyGate::open(&config)?;
        assert!(restarted.current());
        let replacement = node("phone-pin", 3)?;
        let key: String = replacement
            .identity
            .public_key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(&path, format!("{}|{key}\n", phone.id.as_str()))?;
        assert!(
            !restarted.current(),
            "changed key cannot keep static verifier authority"
        );
        std::fs::write(&path, vec![b'x'; POLICY_MAX_BYTES + 1])?;
        assert!(read_peer_pins(&path).is_err());
        std::fs::remove_file(&path)?;
        std::fs::create_dir(&path)?;
        assert!(read_peer_pins(&path).is_err());
        Ok(())
    }

    #[test]
    fn creator_key_fingerprint_survives_unrelated_pin_addition_but_changes_on_rotation()
    -> TestResult {
        let phone = node("phone-history", 2)?;
        let other = node("other-history", 3)?;
        let replacement = node("phone-history", 4)?;
        let row = |identity: &NodeIdentity| {
            let key: String = identity
                .public_key
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("{}|{key}\n", identity.node_id.as_str())
        };
        let original = crate::session_listener::peer_key_fingerprints(&row(&phone.identity));
        let unrelated = crate::session_listener::peer_key_fingerprints(&format!(
            "{}{}",
            row(&phone.identity),
            row(&other.identity)
        ));
        let rotated = crate::session_listener::peer_key_fingerprints(&row(&replacement.identity));
        assert_eq!(
            original.get(phone.id.as_str()),
            unrelated.get(phone.id.as_str())
        );
        assert_ne!(
            original.get(phone.id.as_str()),
            rotated.get(phone.id.as_str())
        );
        assert_eq!(
            original.get(phone.id.as_str()),
            Some(blake3::hash(&phone.identity.public_key).as_bytes())
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn owner_pins_refuse_symlinks() -> TestResult {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("target");
        std::fs::write(&target, "")?;
        let link = temp.path().join("pins");
        std::os::unix::fs::symlink(&target, &link)?;
        assert!(read_peer_pins(&link).is_err());
        Ok(())
    }

    /// A node whose key signs wrapped transcripts (what `AuthHubNodeSigner`
    /// produces in production).
    fn wrapped_node(name: &str, seed: u8) -> TestResult<TestNode> {
        let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32]).map_err(|_| "seed")?;
        let id = NodeId::try_from_str(name)?;
        let identity = NodeIdentity {
            node_id: id.clone(),
            public_key: key.public_key().as_ref().to_vec(),
            key_ref: format!("test-only/{name}"),
        };
        let local = LocalNode::new(
            identity.clone(),
            Arc::new(TranscriptWrappedSigner::new(SeedSigner(key))),
        );
        Ok(TestNode {
            id,
            identity,
            local,
        })
    }

    #[tokio::test]
    async fn a_wrapped_fleet_connects_and_a_plain_peer_is_refused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config = injected_config(temp.path());
        let gateway = wrapped_node("gateway", 1)?;
        let phone = wrapped_node("phone", 2)?;
        let plain = node("plain", 3)?;
        let remote_dir = config.remote_dir();
        std::fs::create_dir_all(&remote_dir)?;
        let registry = DeviceRegistry::new(&remote_dir);
        for (peer, device) in [(&phone, "dev-1"), (&plain, "dev-2")] {
            registry.enroll(&DeviceRecord {
                node_id: peer.id.clone(),
                device: DeviceId::try_from_str(device)?,
                tenant: TenantId::try_from_str("tenant-a")?,
                tier: PermissionTier::Operator,
                revoked: false,
                label: device.to_owned(),
                approve_optin: false,
            })?;
        }
        // The server pins both and verifies wrapped transcripts (fleet-wide).
        let mut peers = PinnedPeers::new();
        peers.pin_identity(&phone.identity)?;
        peers.pin_identity(&plain.identity)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(TranscriptWrappedVerifier::new(peers)),
            listener,
        )
        .await?;
        let client_verifier = TranscriptWrappedVerifier::new(pinned(&gateway.identity)?);
        let connect = |peer: &TestNode| {
            let endpoint = NodeEndpoint {
                addr: ingress.addr(),
                expected_node: gateway.id.clone(),
            };
            let local = peer.local.clone();
            let verifier = client_verifier.clone();
            async move {
                connect_node(endpoint, &local, &verifier, ConnectOptions::new("t"))
                    .await
                    .map(RemotePort::new)
            }
        };
        assert!(connect(&phone).await?.list().await?.is_empty());
        // Exercise the attach connector with the pinned identity and a wrong alias pin.
        let endpoint = ingress.addr().to_string();
        let attached = crate::attach_cmd::connect_authenticated_node(
            "lab",
            &endpoint,
            gateway.id.as_str(),
            &phone.local,
            &client_verifier,
            ConnectOptions::new("attach test"),
        )
        .await?;
        assert!(RemotePort::new(attached).list().await?.is_empty());
        assert!(
            crate::attach_cmd::connect_authenticated_node(
                "lab",
                &endpoint,
                "different-gateway",
                &phone.local,
                &client_verifier,
                ConnectOptions::new("wrong identity"),
            )
            .await
            .is_err()
        );
        // A key that signs the raw (unwrapped) transcript is refused in a
        // wrapped fleet, although it is pinned and enrolled.
        assert!(connect(&plain).await.is_err());
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_unenrolled_node_is_refused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config = injected_config(temp.path());
        let gateway = node("gateway", 1)?;
        let stranger = node("stranger", 3)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&stranger.identity)?),
            listener,
        )
        .await?;
        // Pinned by the transport but not enrolled in the device registry.
        assert!(dial(ingress.addr(), &gateway, &stranger).await.is_err());
        ingress.shutdown().await;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Sharing the profile's sessions (plan Welle 1, acceptance S4)
    // ---------------------------------------------------------------------

    /// The Tui entry requires an active UIA: the same fixture the runtime
    /// tests use (`<home>/profiles/default`).
    fn write_fixture_uia(home: &Path) -> TestResult {
        let profile_dir = home.join("profiles").join("default");
        let agent_dir = profile_dir.join("agents").join("fixture-uia");
        std::fs::create_dir_all(&agent_dir)?;
        std::fs::write(
            agent_dir.join("definition.toml"),
            "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
        )?;
        std::fs::write(
            profile_dir.join("config.toml"),
            "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
        )?;
        Ok(())
    }

    /// Shared-profile mode `continue` over `dir` (home = state = cwd).
    fn share_config(dir: &Path, tenant: &TenantId) -> RemoteServeConfig {
        let mut config = injected_config(dir);
        config.tier = PermissionTier::Operator;
        config.share = SharePlan {
            mode: ShareMode::Continue,
            tenant: Some(tenant.clone()),
            ..SharePlan::off()
        };
        config
    }

    fn enroll(config: &RemoteServeConfig, phone: &TestNode, tenant: &TenantId) -> TestResult {
        std::fs::create_dir_all(config.remote_dir())?;
        DeviceRegistry::new(&config.remote_dir()).enroll(&DeviceRecord {
            node_id: phone.id.clone(),
            device: DeviceId::try_from_str("dev-test-1")?,
            tenant: tenant.clone(),
            tier: PermissionTier::Operator,
            revoked: false,
            label: "phone".into(),
            approve_optin: false,
        })?;
        Ok(())
    }

    /// Like [`dial`], asking for the `projects` feature (a minor-3 app).
    async fn dial_projects(
        addr: SocketAddr,
        gateway: &TestNode,
        phone: &TestNode,
    ) -> Result<RemotePort, harw_session_remote::RemoteError> {
        let verifier = pinned(&gateway.identity)
            .map_err(|error| harw_session_remote::RemoteError::Connect(error.to_string()))?;
        let mut options = ConnectOptions::new("phone");
        options.features = vec![harw_protocol::session_wire::features::PROJECTS.to_owned()];
        let connection = connect_node(
            NodeEndpoint {
                addr,
                expected_node: gateway.id.clone(),
            },
            &phone.local,
            &verifier,
            options,
        )
        .await?;
        Ok(RemotePort::new(connection))
    }

    /// A canonical, existing project folder below `dir`.
    fn project_dir(dir: &Path, name: &str) -> TestResult<PathBuf> {
        let project = dir.join(name);
        std::fs::create_dir_all(&project)?;
        Ok(std::fs::canonicalize(project)?)
    }

    fn text_item(user: bool, text: &str) -> harw_protocol::TurnItem {
        use harw_protocol::{AssistantMessageItem, ContentPart, TurnItem, UserMessageItem};
        let content = vec![ContentPart::Text {
            text: text.to_owned(),
        }];
        if user {
            TurnItem::UserMessage(UserMessageItem {
                id: harw_types::ItemId::new(),
                content,
            })
        } else {
            TurnItem::AssistantMessage(AssistantMessageItem {
                id: harw_types::ItemId::new(),
                content,
                phase: None,
            })
        }
    }

    /// A TUI session in the profile store, written as `harw` writes it
    /// (`cli-session:` thread), with its meta bound to `project`. Returns
    /// the project key.
    async fn seed_tui_session(
        root: &Path,
        id: &SessionId,
        project: &Path,
        text: &str,
    ) -> TestResult<String> {
        use harw_core::{StateStore as _, TranscriptStateStore};
        let store = TranscriptStateStore::new(
            harw_session_store::TranscriptStore::new(root),
            crate::session_serve::cli_session_thread,
        );
        store.save_turn(id, &text_item(true, text)).await?;
        store
            .save_turn(id, &text_item(false, "earlier answer"))
            .await?;
        let key = harw_home::project::project_key(project);
        let bound = key.clone();
        harw_session_store::meta::update(root, id, |meta| {
            meta.turns = meta.turns.max(1);
            if meta.first_user_message.is_none() {
                meta.first_user_message = Some(text.to_owned());
            }
            meta.cwd = Some(project.to_path_buf());
            meta.project_root = Some(project.to_path_buf());
            meta.project_key = Some(bound);
        })?;
        Ok(key)
    }

    /// Fake driver that, like the core, writes the first transcript record
    /// of a new session into the profile store (`cli-session:` thread).
    struct ProfileWritingDriver {
        root: PathBuf,
    }

    impl TurnDriver for ProfileWritingDriver {
        fn prepare_execution(
            &self,
            _: &SessionId,
            _: harw_session_host::driver::ExecutionAdmission,
        ) -> DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn create_session(&self, id: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
            let id = id.clone();
            Box::pin(async move {
                use harw_core::{StateStore as _, TranscriptStateStore};
                TranscriptStateStore::new(
                    harw_session_store::TranscriptStore::new(&self.root),
                    crate::session_serve::cli_session_thread,
                )
                .save_turn(&id, &text_item(true, "first words from the phone"))
                .await
                .map_err(|error| harw_session_host::HostError::Storage(error.to_string()))
            })
        }
        fn run_turn(
            &self,
            _: TurnInput,
            _: CancelSignal,
            _: Arc<dyn EventSink>,
        ) -> DriverFuture<'_, TurnOutcome> {
            Box::pin(async { Ok(TurnOutcome::Completed) })
        }
        fn resume_after_approval(
            &self,
            _: &SessionId,
            _: CancelSignal,
            _: Arc<dyn EventSink>,
        ) -> DriverFuture<'_, TurnOutcome> {
            Box::pin(async { Ok(TurnOutcome::Completed) })
        }
        fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn model_name(&self, _: &SessionId) -> Option<String> {
            None
        }
    }

    /// Acceptance S4: a TUI session continued over the remote host hydrates
    /// from the profile store with the TUI's thread, so the model sees the
    /// earlier entries; nothing lands in the private remote store.
    #[tokio::test]
    async fn tui_session_continued_over_remote_host_sees_earlier_entries() -> TestResult {
        use harw_protocol::session_wire::{AttachParams, SubmitParams, SubmitResult};
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        write_fixture_uia(dir)?;
        let tenant = TenantId::try_from_str("tenant-test")?;
        let config = share_config(dir, &tenant);
        let gateway = node("gateway-share", 1)?;
        let phone = node("phone-share", 2)?;
        enroll(&config, &phone, &tenant)?;
        let root = crate::runtime_entry::profile_sessions_root(dir)?;
        let project = project_dir(dir, "proj")?;
        let id = SessionId::try_from_str("sess-a1")?;
        seed_tui_session(&root, &id, &project, "earlier-entry-marker").await?;

        let model = harw_core::RecordingModelProvider::new();
        let ingress = start_remote(
            &config,
            Arc::new(model.clone()),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        let mobile = dial_projects(ingress.addr(), &gateway, &phone).await?;
        let (ack, frames) = mobile
            .attach(AttachParams {
                session_id: id.clone(),
                from: None,
                profile: Default::default(),
                tail_items: 50,
            })
            .await?;
        assert!(ack.head.durable > 0, "the TUI transcript is readable");
        let submitted = mobile
            .submit(SubmitParams {
                session_id: id.clone(),
                text: "continue-from-phone".to_owned(),
                expect_head: ack.head,
                client_msg_id: "m1".to_owned(),
                force: false,
                images: Vec::new(),
                rewind_before: None,
            })
            .await?;
        assert!(
            matches!(submitted, SubmitResult::Accepted { .. }),
            "{submitted:?}"
        );
        let mut request = None;
        for _ in 0..100 {
            request = model.last();
            if request.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let request = request.ok_or("the model was never called")?;
        let history = format!("{:?}", request.history.items());
        assert!(
            history.contains("earlier-entry-marker"),
            "the model must see the TUI's earlier entries"
        );
        assert!(history.contains("continue-from-phone"));
        assert!(
            !config
                .remote_dir()
                .join(SESSIONS_DIR_NAME)
                .join("sess-a1.jsonl")
                .exists(),
            "a shared session never moves into the private remote store"
        );
        drop((mobile, frames));
        ingress.shutdown().await;
        Ok(())
    }

    /// Acceptance S4: a session the phone creates in a project is a normal
    /// profile session: `harw -r` discovers it, with its project.
    #[tokio::test]
    async fn phone_session_is_discoverable_with_project() -> TestResult {
        use harw_protocol::session_wire::CreateInParams;
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let tenant = TenantId::try_from_str("tenant-test")?;
        let config = share_config(dir, &tenant);
        let gateway = node("gateway-create", 1)?;
        let phone = node("phone-create", 2)?;
        enroll(&config, &phone, &tenant)?;
        let root = crate::runtime_entry::profile_sessions_root(dir)?;
        let project = project_dir(dir, "proj")?;
        // A known project: one TUI session already ran there.
        let key = seed_tui_session(
            &root,
            &SessionId::try_from_str("sess-a1")?,
            &project,
            "tui work",
        )
        .await?;

        let ingress = start_remote_with_driver(
            &config,
            Arc::new(ProfileWritingDriver { root: root.clone() }),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        let mobile = dial_projects(ingress.addr(), &gateway, &phone).await?;
        let created = mobile
            .create_in(CreateInParams {
                project_key: key.clone(),
                title: Some("from the phone".to_owned()),
            })
            .await?;
        assert_eq!(created.tenant.as_ref(), Some(&tenant));

        let discovered = crate::resume::discover_sessions(&root)?;
        let found = discovered
            .iter()
            .find(|session| session.id == created.session_id)
            .ok_or("the phone session is not discoverable from the TUI")?;
        let meta = found.meta.as_ref().ok_or("the phone session has no meta")?;
        assert_eq!(meta.project_key.as_deref(), Some(key.as_str()));
        assert_eq!(meta.project_root.as_deref(), Some(project.as_path()));
        drop(mobile);
        ingress.shutdown().await;
        Ok(())
    }

    /// Acceptance S4: the factory's per-session working directory is the
    /// session's checked project root; unknown sessions fall back (`None`).
    #[tokio::test]
    async fn session_runs_in_project_dir() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("sessions");
        std::fs::create_dir_all(&root)?;
        let project = project_dir(temp.path(), "proj")?;
        let id = SessionId::try_from_str("sess-a1")?;
        seed_tui_session(&root, &id, &project, "work here").await?;
        let index = Arc::new(FsProfileIndex::open(root, IndexOptions::default()));
        let resolve = workdir_resolver(index);
        assert_eq!(resolve(&id), Some(project));
        assert_eq!(resolve(&SessionId::try_from_str("sess-unknown")?), None);
        Ok(())
    }

    /// Regression guard: with sharing off the remote host keeps its private
    /// store and never touches the profile's sessions directory.
    #[tokio::test]
    async fn share_off_keeps_remote_store() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let config = injected_config(dir);
        assert!(matches!(config.share.mode, ShareMode::Off));
        assert!(open_share(&config)?.is_none());
        assert_eq!(
            transcript_root(&config, None)?,
            config.remote_dir().join(SESSIONS_DIR_NAME)
        );
        let gateway = node("gateway-off", 1)?;
        let phone = node("phone-off", 2)?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        assert!(config.remote_dir().join(SESSIONS_DIR_NAME).is_dir());
        assert!(
            !dir.join("profiles")
                .join("default")
                .join("sessions")
                .exists(),
            "share off must not create or use the profile store"
        );
        ingress.shutdown().await;
        Ok(())
    }

    /// Sharing without a tenant is a startup error, never a panic.
    #[test]
    fn share_without_tenant_is_an_error() -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = injected_config(temp.path());
        config.share = SharePlan {
            mode: ShareMode::Read,
            tenant: None,
            ..SharePlan::off()
        };
        assert!(open_share(&config).is_err());
        Ok(())
    }
}
