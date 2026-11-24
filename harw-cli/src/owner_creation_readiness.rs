//! Explicit, read-only local operator check for one guarded writer receipt.
//! Nothing is enrolled, assigned, transferred, or made reachable here.

use std::fs::{File, Metadata};
use std::io::{self, IsTerminal, Read as _, Write as _};
use std::num::NonZeroU64;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harw_cloud_gateway::owner_create_receipt::{
    MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES, OwnerCreationKeyPin, OwnerCreationObserveKey,
    OwnerCreationPinScope,
};
use harw_cloud_gateway::owner_readiness::OwnerReadinessPin;
use harw_fsutil::{
    OpenMode, ensure_owned_directory, ensure_private_regular, open_beneath, open_dir_at,
    open_dir_nofollow, same_file_at,
};
use harw_infra_client::{
    AuthHubClient, BearerToken, ClientOptions as InfraOptions, KeyRef, MAX_TOKEN_LEN, OwnerEpochClient,
};
use harw_node_transport::{
    AuthHubNodeSigner, ClientOptions as NodeOptions, LocalNode, NodeIdentity, NodeTransportClient,
    PinnedPeers, TranscriptWrappedVerifier, node_key_fingerprint,
};
use harw_session_host::owner_creation::ML_DSA_65_PUBLIC_KEY_LEN;
use harw_types::NodeId;
use serde::Deserialize;
use tokio::net::UnixStream;
use zeroize::Zeroizing;

use crate::session_listener::{HubSign, NODE_IDENTITY_NAMESPACE};

const PIN_BYTES: usize = 32 * 1024;
const TOKEN_BYTES: usize = MAX_TOKEN_LEN + 64;
const WRITER_DIRECTORY: &str = "private-writer-v1";
const WRITER_SOCKET: &str = "readiness-v2.sock";
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    node: String,
    instance: String,
    epoch: NonZeroU64,
    namespace_id_hex: String,
    receipt_public_key_hex: String,
    node_public_key_hex: String,
    readiness_public_key_hex: String,
    observe: ObservePin,
    hub_node: String,
    hub_key_blake3: String,
    writer_socket: PathBuf,
    auth_socket: PathBuf,
    auth_socket_owner_uid: u32,
    auth_peer_uid: u32,
    auth_token_file: Option<PathBuf>,
    netsec_socket: PathBuf,
    netsec_socket_owner_uid: u32,
    netsec_peer_uid: u32,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum ObservePin {
    Disabled,
}

fn invalid_config() -> String {
    "Owner readiness: invalid independent private configuration".to_owned()
}

fn unavailable() -> String {
    "Owner readiness: the selected local authorities or guarded proof lane are unavailable"
        .to_owned()
}

fn changed() -> String {
    "Owner readiness: selected configuration or socket identity changed during verification"
        .to_owned()
}

fn decode<const N: usize>(input: &str) -> Result<[u8; N], String> {
    if input.len() != N * 2 {
        return Err(invalid_config());
    }
    data_encoding::HEXLOWER.decode(input.as_bytes()).map_err(|_| invalid_config())?
        .try_into().map_err(|_| invalid_config())
}

fn valid_node(node: &str) -> bool {
    !node.is_empty() && node.len() <= 128 && !node.starts_with('.')
        && node.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn trusted_parent(path: &Path, owner: u32) -> Result<File, String> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || !components.all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(invalid_config());
    }
    let validate = |directory: &File| {
        let meta = directory.metadata().map_err(|_| invalid_config())?;
        if !meta.is_dir() || (meta.uid() != 0 && meta.uid() != owner) || meta.mode() & 0o022 != 0 {
            return Err(invalid_config());
        }
        Ok(())
    };
    let parent = path.parent().ok_or_else(invalid_config)?;
    let mut directory = File::from(open_dir_nofollow(Path::new("/")).map_err(|_| invalid_config())?);
    validate(&directory)?;
    for component in parent.components().skip(1) {
        let Component::Normal(name) = component else { return Err(invalid_config()); };
        directory = open_dir_at(directory.as_fd(), Path::new(name)).map_err(|_| invalid_config())?;
        validate(&directory)?;
    }
    Ok(directory)
}

fn snapshot_identity(meta: &Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

fn same_file_snapshot(a: &Metadata, b: &Metadata) -> bool {
    snapshot_identity(a) == snapshot_identity(b)
        && a.uid() == b.uid() && a.mode() == b.mode() && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
        && a.mtime() == b.mtime() && a.mtime_nsec() == b.mtime_nsec()
}

struct PrivateFile {
    path: PathBuf,
    file: File,
    meta: Metadata,
    parent_id: (u64, u64),
    owner: u32,
}

impl PrivateFile {
    fn open(path: &Path, owner: u32, max: usize) -> Result<Self, String> {
        let parent = trusted_parent(path, owner)?;
        ensure_owned_directory(&parent).map_err(|_| invalid_config())?;
        let parent_meta = parent.metadata().map_err(|_| invalid_config())?;
        if parent_meta.mode() & 0o077 != 0 {
            return Err(invalid_config());
        }
        let name = Path::new(path.file_name().ok_or_else(invalid_config)?);
        let file = open_beneath(parent.as_fd(), name, OpenMode::read_only())
            .map_err(|_| invalid_config())?;
        ensure_private_regular(&file).map_err(|_| invalid_config())?;
        let meta = file.metadata().map_err(|_| invalid_config())?;
        if meta.nlink() != 1 || meta.len() == 0 || meta.len() > max as u64 {
            return Err(invalid_config());
        }
        let pinned = Self { path: path.to_owned(), file, meta, parent_id: snapshot_identity(&parent_meta), owner };
        pinned.check()?;
        Ok(pinned)
    }

    fn check(&self) -> Result<(), String> {
        let parent = trusted_parent(&self.path, self.owner)?;
        ensure_owned_directory(&parent).map_err(|_| changed())?;
        let parent_meta = parent.metadata().map_err(|_| changed())?;
        let name = Path::new(self.path.file_name().ok_or_else(changed)?);
        if parent_meta.mode() & 0o077 != 0
            || snapshot_identity(&parent_meta) != self.parent_id
            || !same_file_at(parent.as_fd(), name, &self.file).map_err(|_| changed())?
            || !same_file_snapshot(&self.meta, &self.file.metadata().map_err(|_| changed())?)
        {
            return Err(changed());
        }
        Ok(())
    }

    // All file contents passed here are exact-size, bounded and zeroizing.
    // In particular no token bytes are copied into an ordinary Vec.
    fn bytes(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        let mut bytes = Zeroizing::new(vec![0u8; self.meta.len() as usize]);
        let mut reader = &self.file;
        reader.read_exact(&mut bytes[..]).map_err(|_| invalid_config())?;
        let mut more = Zeroizing::new([0u8; 1]);
        if reader.read(&mut more[..]).map_err(|_| invalid_config())? != 0 {
            return Err(changed());
        }
        self.check()?;
        Ok(bytes)
    }
}

struct SocketPin {
    path: PathBuf,
    owner: u32,
    writer: bool,
    parent_id: (u64, u64),
    socket: Metadata,
}

impl SocketPin {
    fn capture(path: &Path, owner: u32, writer: bool) -> Result<Self, String> {
        let (parent_id, socket) = Self::metadata(path, owner, writer)?;
        Ok(Self { path: path.to_owned(), owner, writer, parent_id, socket })
    }

    fn metadata(path: &Path, owner: u32, writer: bool) -> Result<((u64, u64), Metadata), String> {
        let parent_path = path.parent().ok_or_else(invalid_config)?;
        let basename = path.file_name().ok_or_else(invalid_config)?;
        let parent = trusted_parent(path, owner)?;
        let parent_meta = parent.metadata().map_err(|_| unavailable())?;
        let anchored = PathBuf::from("/proc/self/fd").join(parent.as_raw_fd().to_string()).join(basename);
        let socket = std::fs::symlink_metadata(anchored).map_err(|_| unavailable())?;
        if !socket.file_type().is_socket() || socket.nlink() != 1 || socket.uid() != owner
            || socket.mode() & 0o002 != 0
            || (writer && (parent_path.file_name().and_then(|name| name.to_str()) != Some(WRITER_DIRECTORY)
                || basename.to_str() != Some(WRITER_SOCKET)
                || parent_meta.uid() != owner || parent_meta.mode() & 0o777 != 0o700
                || socket.mode() & 0o777 != 0o600))
        {
            return Err(invalid_config());
        }
        Ok((snapshot_identity(&parent_meta), socket))
    }

    fn check(&self) -> Result<(), String> {
        let (parent, current) = Self::metadata(&self.path, self.owner, self.writer)?;
        if parent != self.parent_id || !same_file_snapshot(&self.socket, &current) {
            return Err(changed());
        }
        Ok(())
    }
}

/// Evidence arrives on stdin; neither identity nor address is derived from it.
/// Only independently selected local services are contacted. No route,
/// assignment, key generation or trust changes are available in this mode.
pub(crate) fn run(pin_path: &Path, tenant: &str, session: &str) -> Result<(), String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Err("Owner readiness: supply the creation receipt JSON on stdin".into());
    }
    let mut receipt = Vec::new();
    stdin.lock().take((MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES + 1) as u64)
        .read_to_end(&mut receipt).map_err(|_| "Owner readiness: receipt input unavailable")?;
    if receipt.is_empty() || receipt.len() > MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES {
        return Err("Owner readiness: receipt input has an invalid length".into());
    }

    let uid = rustix::process::geteuid().as_raw();
    let config_file = PrivateFile::open(pin_path, uid, PIN_BYTES)?;
    let config: Config = serde_json::from_slice(&config_file.bytes()?).map_err(|_| invalid_config())?;
    if config.version != 1 || !valid_node(&config.node) || !valid_node(&config.hub_node)
        || config.hub_node == config.node
        || config.netsec_peer_uid == uid
    {
        return Err(invalid_config());
    }
    let ObservePin::Disabled = config.observe;
    let owner_node = NodeId::try_from_str(&config.node).map_err(|_| invalid_config())?;
    let hub_node = NodeId::try_from_str(&config.hub_node).map_err(|_| invalid_config())?;
    let namespace_id = decode::<32>(&config.namespace_id_hex)?;
    let hub_pin = decode::<32>(&config.hub_key_blake3)?;
    let receipt_key = decode::<ML_DSA_65_PUBLIC_KEY_LEN>(&config.receipt_public_key_hex)?;
    let owner_key = decode::<ML_DSA_65_PUBLIC_KEY_LEN>(&config.node_public_key_hex)?;
    let readiness_key = decode::<ML_DSA_65_PUBLIC_KEY_LEN>(&config.readiness_public_key_hex)?;
    if hub_pin == [0; 32] || [&receipt_key, &owner_key, &readiness_key].into_iter()
        .any(|key| node_key_fingerprint(key) == Some(hub_pin))
    {
        return Err(invalid_config());
    }
    let scope = || OwnerCreationPinScope {
        node: &config.node, instance: &config.instance, epoch: config.epoch, namespace_id,
    };
    let receipt_pin = OwnerCreationKeyPin::new(scope(), &receipt_key, &owner_key, OwnerCreationObserveKey::Disabled)
        .map_err(|_| invalid_config())?;
    let receipt = receipt_pin.verify_json(tenant, session, &receipt).map_err(|error| error.to_string())?;
    let readiness_pin = OwnerReadinessPin::new(
        scope(), &readiness_key, &owner_key, &receipt_key, OwnerCreationObserveKey::Disabled,
    ).map_err(|_| invalid_config())?;

    let writer = SocketPin::capture(&config.writer_socket, uid, true)?;
    let auth_socket = SocketPin::capture(&config.auth_socket, config.auth_socket_owner_uid, false)?;
    let _root_controlled_netsec = trusted_parent(&config.netsec_socket, 0)?;
    let netsec_socket = SocketPin::capture(&config.netsec_socket, config.netsec_socket_owner_uid, false)?;
    let ids = [snapshot_identity(&writer.socket), snapshot_identity(&auth_socket.socket), snapshot_identity(&netsec_socket.socket)];
    if ids[0] == ids[1] || ids[0] == ids[2] || ids[1] == ids[2] {
        return Err(invalid_config());
    }
    let token_file = config.auth_token_file.as_deref().map(|path| PrivateFile::open(path, uid, TOKEN_BYTES))
        .transpose()?;
    let token = token_file.as_ref().map(|file| {
        BearerToken::from_secret(file.bytes()?).map_err(|_| invalid_config())
    }).transpose()?;
    let check = || {
        config_file.check()?;
        if let Some(file) = &token_file {
            file.check()?;
        }
        writer.check()?;
        auth_socket.check()?;
        let _root_controlled_netsec = trusted_parent(&netsec_socket.path, 0)?;
        netsec_socket.check()
    };
    check()?;
    // Filesystem inode ownership and the auth service's kernel peer UID
    // are separate pins. On socket activation the latter may be PID1.
    let auth = AuthHubClient::new_uid_pinned(
        config.auth_socket, config.auth_peer_uid, token, InfraOptions::default(),
    ).map_err(|_| invalid_config())?;
    let netsec = OwnerEpochClient::new(config.netsec_socket, config.netsec_peer_uid)
        .map_err(|_| invalid_config())?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
        .map_err(|_| unavailable())?;
    runtime.block_on(async {
        let key = KeyRef::latest(NODE_IDENTITY_NAMESPACE, hub_node.as_str()).map_err(|_| invalid_config())?;
        let public = auth.public_key(&key).await.map_err(|_| unavailable())?.into_vec();
        if node_key_fingerprint(&public) != Some(hub_pin) {
            return Err(invalid_config());
        }
        let signer = AuthHubNodeSigner::new(Arc::new(HubSign(auth)), &hub_node)
            .and_then(|signer| signer.with_self_check(public.clone()))
            .map_err(|_| invalid_config())?;
        let hub = LocalNode::new(NodeIdentity {
            key_ref: format!("{NODE_IDENTITY_NAMESPACE}/{}", hub_node.as_str()),
            node_id: hub_node,
            public_key: public,
        }, Arc::new(signer));
        let mut peers = PinnedPeers::new();
        peers.pin(owner_node.clone(), owner_key.to_vec()).map_err(|_| invalid_config())?;
        let verifier = TranscriptWrappedVerifier::new(peers);
        check()?;
        let stream = tokio::time::timeout(DIAL_TIMEOUT, UnixStream::connect(&writer.path))
            .await.map_err(|_| unavailable())?.map_err(|_| unavailable())?;
        if stream.peer_cred().map_err(|_| unavailable())?.uid() != uid {
            return Err(unavailable());
        }
        writer.check()?;
        let mut node_options = NodeOptions::default();
        node_options.handshake_timeout = HANDSHAKE_TIMEOUT;
        let mut client = NodeTransportClient::connect_over(stream, &hub, &owner_node, &verifier, &node_options)
            .await.map_err(|_| unavailable())?;
        check()?;
        let proof = readiness_pin.query(&hub, &mut client, &netsec, &receipt).await
            .map_err(|error| error.to_string())?;
        check()?;
        proof.confirm_current(&receipt).await.map_err(|error| error.to_string())?;
        check()?;
        // The client stays alive through the final observation and output.
        if client.owner_process_channel_binding().is_none() {
            return Err(unavailable());
        }
        writeln!(
            io::stdout().lock(),
            "Guarded writer readiness verified at this check. No ownership was assigned.",
        ).map_err(|_| "Owner readiness: could not write verification result".to_owned())
    })
}
