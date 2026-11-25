//! Exact, explicit local operator configuration. Nothing here activates a
//! listener, generates a credential, or falls back to the legacy epoch API.

use std::fs::{File, Metadata};
use std::io::Read;
use std::os::fd::AsFd as _;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use harw_fsutil::{ensure_owned_directory, ensure_private_regular, open_dir_at,
    open_dir_nofollow, same_file_at};
use harw_node_transport::{LocalNode, ML_DSA_65_PUBLIC_KEY_LEN};
use harw_session_host::SessionHost;
use harw_types::{NodeId, SessionId, TenantId};
use serde::Deserialize;
use zeroize::Zeroizing;

use super::{GuardedWriterReadiness, WriterReadinessPin};
use super::super::{Pin as CreationPin, RECEIPT_PIN};

const READINESS_PIN: &str = "writer-readiness-pin-v1.json";
const READINESS_CREDENTIAL: &str = "harw-private-writer-readiness-seed";
const READINESS_SCOPE: &str = "writer-readiness-scope-v1.json";
const MAX_SCOPE_BYTES: usize = 2 * 1024;
const MAX_PIN_BYTES: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    mode: Mode,
    owner_node: NodeId,
    hub_node: NodeId,
    instance: String,
    namespace_id_hex: String,
    hub_public_key_hex: String,
    receipt_public_key_hex: String,
    readiness_public_key_hex: String,
    netsec_socket: PathBuf,
    netsec_peer_uid: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    ColocatedNetsecV2,
}

fn invalid() -> String {
    "isolated writer readiness requires exact protected co-located NetSec configuration".into()
}

fn token(text: &str, limit: usize, colon: bool) -> bool {
    !text.is_empty() && text.len() <= limit && !text.starts_with('.')
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric()
            || matches!(byte, b'.' | b'_' | b'-') || (colon && byte == b':'))
}

fn fixed<const N: usize>(text: &str) -> Result<[u8; N], String> {
    if text.len() != N * 2 {
        return Err(invalid());
    }
    data_encoding::HEXLOWER.decode(text.as_bytes()).map_err(|_| invalid())?
        .try_into().map_err(|_| invalid())
}

fn trusted_directory(path: &Path, owner: u32) -> Result<File, String> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(invalid());
    }
    let mut dir = File::from(open_dir_nofollow(Path::new("/")).map_err(|_| invalid())?);
    let check = |dir: &File| {
        let meta = dir.metadata().map_err(|_| invalid())?;
        if !meta.is_dir() || (meta.uid() != 0 && meta.uid() != owner) || meta.mode() & 0o022 != 0 {
            return Err(invalid());
        }
        Ok(())
    };
    check(&dir)?;
    for part in components {
        let Component::Normal(name) = part else { return Err(invalid()); };
        dir = open_dir_at(dir.as_fd(), Path::new(name)).map_err(|_| invalid())?;
        check(&dir)?;
    }
    Ok(dir)
}

fn private_directory(path: &Path) -> Result<File, String> {
    let dir = trusted_directory(path, rustix::process::geteuid().as_raw())?;
    ensure_owned_directory(&dir).map_err(|_| invalid())?;
    if dir.metadata().map_err(|_| invalid())?.mode() & 0o077 != 0 {
        return Err(invalid());
    }
    Ok(dir)
}

fn same_snapshot(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino() && a.uid() == b.uid()
        && a.gid() == b.gid() && a.mode() == b.mode() && a.nlink() == b.nlink()
        && a.len() == b.len() && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
        && a.mtime() == b.mtime() && a.mtime_nsec() == b.mtime_nsec()
}

struct PrivateSnapshot {
    parent: PathBuf,
    name: &'static str,
    file: File,
    meta: Metadata,
}

impl PrivateSnapshot {
    fn open(parent: &Path, name: &'static str, max: usize) -> Result<Self, String> {
        let dir = private_directory(parent)?;
        // One fixed component, no symlink-following; NONBLOCK avoids startup
        // hanging if an invalid local file is a named pipe.
        let fd = rustix::fs::openat(&dir, name, rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK, rustix::fs::Mode::empty())
            .map_err(|_| invalid())?;
        let file = File::from(fd);
        ensure_private_regular(&file).map_err(|_| invalid())?;
        let meta = file.metadata().map_err(|_| invalid())?;
        if meta.nlink() != 1 || !(1..=max as u64).contains(&meta.len()) {
            return Err(invalid());
        }
        let pin = Self { parent: parent.to_owned(), name, file, meta };
        pin.current()?;
        Ok(pin)
    }

    fn current(&self) -> Result<(), String> {
        let dir = private_directory(&self.parent)?;
        let fresh = self.file.metadata().map_err(|_| invalid())?;
        if !same_snapshot(&fresh, &self.meta)
            || !same_file_at(dir.as_fd(), Path::new(self.name), &self.file)
                .map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        Ok(())
    }

    fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        let mut data = Vec::new();
        (&self.file).take(self.meta.len() + 1).read_to_end(&mut data).map_err(|_| invalid())?;
        if data.len() as u64 != self.meta.len() {
            return Err(invalid());
        }
        self.current()?;
        serde_json::from_slice(&data).map_err(|_| invalid())
    }

    fn seed(&self) -> Result<Zeroizing<[u8; 32]>, String> {
        if self.meta.len() != 32 || self.meta.mode() & 0o222 != 0 {
            return Err(invalid());
        }
        let mut seed = Zeroizing::new([0; 32]);
        let mut file = &self.file;
        file.read_exact(&mut seed[..]).map_err(|_| invalid())?;
        let mut extra = Zeroizing::new([0u8; 1]);
        if file.read(&mut extra[..]).map_err(|_| invalid())? != 0 {
            return Err(invalid());
        }
        self.current()?;
        Ok(seed)
    }
}

pub(super) struct ReadinessConfigurationPin {
    configuration: PrivateSnapshot,
    receipt: PrivateSnapshot,
    credential: PrivateSnapshot,
}

impl ReadinessConfigurationPin {
    pub(super) fn current(&self) -> bool {
        self.configuration.current().is_ok() && self.receipt.current().is_ok()
            && self.credential.current().is_ok()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeConfig {
    version: u8,
    tenant: TenantId,
    session: SessionId,
}

/// One exact record armed by trusted local configuration, never by the
/// remote challenge. A restart isn't required to select a new actual receipt.
pub(super) struct ArmedReceiptScope {
    pub(super) tenant: TenantId,
    pub(super) session: SessionId,
    source: PrivateSnapshot,
}

impl ArmedReceiptScope {
    pub(super) fn read(remote: &Path) -> Result<Self, String> {
        let source = PrivateSnapshot::open(remote, READINESS_SCOPE, MAX_SCOPE_BYTES)?;
        let config: ScopeConfig = source.json()?;
        if config.version != 1 || !token(config.tenant.as_str(), 255, true)
            || !token(config.session.as_str(), 128, false)
        {
            return Err(invalid());
        }
        Ok(Self { tenant: config.tenant, session: config.session, source })
    }

    pub(super) fn current(&self) -> bool {
        self.source.current().is_ok()
    }
}

pub(in crate::session_serve_remote) struct ConfiguredReadiness {
    pub(in crate::session_serve_remote) producer: Arc<GuardedWriterReadiness>,
    pub(in crate::session_serve_remote) hub: NodeId,
    pub(in crate::session_serve_remote) hub_public_key: Vec<u8>,
}

fn trusted_netsec_socket(path: &Path, peer_uid: u32) -> Result<(), String> {
    // NetSec must be administratively separate from the writer's runtime UID.
    // Only root can replace entries in the boot-selected socket ancestry.
    if peer_uid == rustix::process::geteuid().as_raw() {
        return Err(invalid());
    }
    let parent = path.parent().ok_or_else(invalid)?;
    let _protected = trusted_directory(parent, 0)?;
    let meta = path.symlink_metadata().map_err(|_| invalid())?;
    // A socket-activated service can chown the inode to its service
    // user while the listener's kernel peer credentials belong to PID 1.
    // Protected ancestry authenticates the path binding; OwnerEpochClient
    // independently checks the connected socket's kernel UID on every read.
    if !meta.file_type().is_socket() || meta.mode() & 0o002 != 0 {
        return Err(invalid());
    }
    Ok(())
}

/// Open only an explicitly configured producer. The protected NetSec socket
/// and kernel UID are necessary but insufficient; current daemon identity
/// is independently sampled via V2 for each challenge and signed locally.
pub(in crate::session_serve_remote) fn open_configured(
    host: SessionHost,
    remote: &Path,
    local: &LocalNode,
) -> Result<ConfiguredReadiness, String> {
    let configuration = PrivateSnapshot::open(remote, READINESS_PIN, MAX_PIN_BYTES)?;
    let config: Config = configuration.json()?;
    let Mode::ColocatedNetsecV2 = config.mode;
    if config.version != 1 || &config.owner_node != local.node_id()
        || !token(config.owner_node.as_str(), 128, false) || !token(config.hub_node.as_str(), 128, false)
        || !token(&config.instance, 255, true)
    {
        return Err(invalid());
    }
    trusted_netsec_socket(&config.netsec_socket, config.netsec_peer_uid)?;
    let namespace_id = fixed::<32>(&config.namespace_id_hex)?;
    let hub_public_key = fixed::<ML_DSA_65_PUBLIC_KEY_LEN>(&config.hub_public_key_hex)?.to_vec();
    let receipt_public_key = fixed::<ML_DSA_65_PUBLIC_KEY_LEN>(&config.receipt_public_key_hex)?.to_vec();
    let readiness_public_key = fixed::<ML_DSA_65_PUBLIC_KEY_LEN>(&config.readiness_public_key_hex)?.to_vec();

    let receipt = PrivateSnapshot::open(remote, RECEIPT_PIN, MAX_PIN_BYTES)?;
    let existing: CreationPin = receipt.json()?;
    if existing.node != config.owner_node.as_str() || existing.instance != config.instance
        || existing.receipt_public_key_hex != config.receipt_public_key_hex
    {
        return Err(invalid());
    }
    let credential_dir = std::env::var_os("CREDENTIALS_DIRECTORY").ok_or_else(invalid)?;
    let credential = PrivateSnapshot::open(Path::new(&credential_dir), READINESS_CREDENTIAL, 32)?;
    let seed = credential.seed()?;
    let policy = ReadinessConfigurationPin { configuration, receipt, credential };
    if !policy.current() {
        return Err(invalid());
    }
    let hub = config.hub_node;
    let pin = WriterReadinessPin {
        hub: hub.clone(),
        hub_public_key: hub_public_key.clone(),
        instance: config.instance,
        namespace_id,
        receipt_public_key,
        readiness_public_key,
    };
    let producer = GuardedWriterReadiness::open(
        host, remote, local, pin, &config.netsec_socket, config.netsec_peer_uid, &seed, policy,
    ).map_err(|_| invalid())?;
    Ok(ConfiguredReadiness { producer: Arc::new(producer), hub, hub_public_key })
}
