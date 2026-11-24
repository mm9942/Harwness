//! Explicit Linux startup for the fixed read-only placement Hub.
//!
//! The ordinary gateway, Pingora proxy, and session listener are unaffected.
//! One startup mode selects loopback or Onion, never both. No forwarded
//! traffic, owner assignment, provisioning, discovery or fallback is provided.
//! Independent deployment authority must provision state and trust pins.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use harw_cloud_gateway::owner_direct_link::{ConfiguredOwnerDirectLink, OwnerDirectLinks};
use harw_cloud_gateway::owner_directory::OwnerDirectory;
use harw_cloud_gateway::owner_ingress::HubTransportIngress;
use harw_cloud_gateway::owner_placement_listener::serve_owner_placement_listener;
#[cfg(feature = "owner-placement-hub-onion")]
use harw_cloud_gateway::owner_placement_listener::serve_owner_placement_onion;
use harw_cloud_gateway::owner_process_proof::{DestinationProcessKeyPin, OwnerProcessTrustSet};
use harw_cloud_gateway::owner_proof_exchange::PinnedOwnerPlacementProofExchange;
use harw_cloud_gateway::owner_session_grants::ExplicitDirectObserveGrants;
use harw_fsutil::{OpenMode, ensure_owned_directory, ensure_private_regular, open_beneath, open_dir_nofollow};
use harw_infra_client::{AuthHubClient, BearerToken, ClientOptions, KeyRef, OwnerEpochClient};
use harw_node_transport::{
    AuthHubNodeSigner, LocalNode, NodeIdentity, NodeTransportServer, PinnedPeers, ServerOptions,
    TranscriptWrappedVerifier, node_key_fingerprint,
};
#[cfg(feature = "owner-placement-hub-onion")]
use harw_onion::{OnionClient, OnionClientConfig, OnionLocator, OnionServerConfig};
use harw_types::NodeId;
use serde::Deserialize;

use crate::session_listener::{HubSign, NODE_IDENTITY_NAMESPACE, PEERS_FILE, decode_hex};

const CONFIG_MAX: u64 = 512 * 1024;
const PEERS_MAX: u64 = 1024 * 1024;
const MAX_OWNERS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    listen: Option<SocketAddr>,
    onion: Option<ConfiguredOnion>,
    profile: String,
    node_id: String,
    hub_key_blake3: String,
    auth_socket: PathBuf,
    auth_peer_uid: u32,
    auth_token_file: Option<PathBuf>,
    netsec_socket: PathBuf,
    netsec_peer_uid: u32,
    owner_directory: PathBuf,
    observe_policy_directory: PathBuf,
    observe_policy_revision: NonZeroU64,
    observe_policy_sha256: String,
    owners: Vec<Owner>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    node_id: String,
    instance: String,
    epoch: NonZeroU64,
    proof_addr: Option<SocketAddr>,
    proof_onion: Option<String>,
    process_public_key_hex: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredOnion {
    locator: String,
    service_state: PathBuf,
    service_cache: PathBuf,
    outbound_state: PathBuf,
    outbound_cache: PathBuf,
}

enum HubRoute {
    Loopback(SocketAddr),
    #[cfg(feature = "owner-placement-hub-onion")]
    Onion {
        locator: OnionLocator,
        server: OnionServerConfig,
        client: OnionClientConfig,
    },
}

impl HubRoute {
    fn select(config: &Config) -> Result<Self, String> {
        match (config.listen, config.onion.as_ref()) {
            (Some(address), None) if loopback_binding(address) => Ok(Self::Loopback(address)),
            (None, Some(onion)) => Self::select_onion(onion),
            _ => Err("Owner Hub: choose exactly one loopback listen or Onion mode".into()),
        }
    }

    #[cfg(not(feature = "owner-placement-hub-onion"))]
    fn select_onion(onion: &ConfiguredOnion) -> Result<Self, String> {
        // Serde still recognizes the explicit stanza on non-Onion builds;
        // consume its fields so the config failure stays precise and inert.
        let ConfiguredOnion { locator, service_state, service_cache, outbound_state, outbound_cache } = onion;
        let _ = (locator, service_state, service_cache, outbound_state, outbound_cache);
        Err("Owner Hub: the Onion mode needs a build with owner-placement-hub-onion".into())
    }

    #[cfg(feature = "owner-placement-hub-onion")]
    fn select_onion(onion: &ConfiguredOnion) -> Result<Self, String> {
        let locator = onion.locator.parse::<OnionLocator>()
            .map_err(|_| "Owner Hub: invalid expected v3 Onion locator")?;
        let paths = [
            &onion.service_state, &onion.service_cache, &onion.outbound_state, &onion.outbound_cache,
        ];
        let directory_error = "Owner Hub: Onion state/cache directories must be distinct, existing and private";
        let mut inodes = BTreeSet::new();
        for (index, path) in paths.iter().enumerate() {
            if paths[..index].iter().any(|other| path.starts_with(other) || other.starts_with(path)) {
                return Err(directory_error.into());
            }
            let directory = private_directory(path).ok_or(directory_error)?;
            let metadata = directory.metadata().map_err(|_| directory_error)?;
            if !inodes.insert((metadata.dev(), metadata.ino())) {
                return Err(directory_error.into());
            }
        }
        let mut server = OnionServerConfig::new(&onion.service_state, &onion.service_cache);
        server.nickname = "harw-owner-placement-hub".to_owned();
        server.virtual_port = locator.port();
        let client = OnionClientConfig::new(&onion.outbound_state, &onion.outbound_cache);
        Ok(Self::Onion { locator, server, client })
    }

    fn owner_link(
        &self,
        owner: &Owner,
        identity: NodeIdentity,
        destinations: &mut BTreeSet<String>,
    ) -> Result<ConfiguredOwnerDirectLink, String> {
        let invalid = "Owner Hub: invalid, repeated or mixed-mode owner proof endpoint";
        match (self, owner.proof_addr, owner.proof_onion.as_deref()) {
            (Self::Loopback(local), Some(address), None) if loopback_binding(address)
                && address != *local && destinations.insert(address.to_string()) =>
                ConfiguredOwnerDirectLink::new(address, identity).map_err(|_| invalid.into()),
            #[cfg(feature = "owner-placement-hub-onion")]
            (Self::Onion { locator, .. }, None, Some(remote)) => {
                let remote = remote.parse::<OnionLocator>().map_err(|_| invalid)?;
                if remote.host() == locator.host() || !destinations.insert(remote.to_string()) {
                    return Err(invalid.into());
                }
                ConfiguredOwnerDirectLink::new_onion(remote, identity).map_err(|_| invalid.into())
            }
            _ => Err(invalid.into()),
        }
    }
}

fn plain_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.components().skip(1).all(|part| matches!(part, Component::Normal(_)))
}

fn loopback_binding(addr: SocketAddr) -> bool {
    addr.ip().is_loopback() && addr.port() != 0
        && match addr {
            SocketAddr::V4(_) => true,
            SocketAddr::V6(v6) => v6.scope_id() == 0 && v6.flowinfo() == 0,
        }
}

fn fixed_pin(raw: &str) -> Option<[u8; 32]> {
    let pin: [u8; 32] = decode_hex(raw)?.try_into().ok()?;
    (pin != [0; 32]).then_some(pin)
}

/// Anchor at the root and reject symlinks in every component. The
/// operator still needs to control the directory ancestry.
fn private_directory(path: &Path) -> Option<File> {
    if !plain_absolute(path) {
        return None;
    }
    let root = open_dir_nofollow(Path::new("/")).ok()?;
    let relative = path.strip_prefix("/").ok()?;
    let directory = File::from(open_beneath(
        root.as_fd(),
        if relative.as_os_str().is_empty() { Path::new(".") } else { relative },
        OpenMode::read_only(),
    ).ok()?);
    ensure_owned_directory(&directory).ok()?;
    (directory.metadata().ok()?.mode() & 0o077 == 0).then_some(directory)
}

/// Open the selected file below the checked and pinned parent FD.
fn read_private(path: &Path, limit: u64) -> Option<String> {
    if !plain_absolute(path) {
        return None;
    }
    let parent = private_directory(path.parent()?)?;
    let file = open_beneath(parent.as_fd(), Path::new(path.file_name()?), OpenMode::read_only()).ok()?;
    ensure_private_regular(&file).ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.nlink() != 1 || metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn read_exact_peers(
    path: &Path,
    local: &NodeId,
    hub_pin: [u8; 32],
) -> Result<(PinnedPeers, BTreeMap<String, Vec<u8>>), String> {
    let input = read_private(path, PEERS_MAX).ok_or("Owner Hub peer pins are unavailable")?;
    let mut peers = PinnedPeers::new();
    let mut by_node = BTreeMap::new();
    let mut seen_keys = BTreeSet::new();
    let invalid = "Owner Hub peer pins are invalid or ambiguous";
    for line in input.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (node, public_key) = line.split_once('|').ok_or(invalid)?;
        let id = NodeId::try_from_str(node.trim()).map_err(|_| invalid)?;
        let key = decode_hex(public_key.trim()).ok_or(invalid)?;
        let key_pin = node_key_fingerprint(&key).ok_or(invalid)?;
        if &id == local || key_pin == hub_pin || !seen_keys.insert(key_pin)
            || by_node.insert(id.as_str().to_owned(), key.clone()).is_some()
        {
            return Err(invalid.into());
        }
        peers.pin(id, key).map_err(|_| invalid)?;
    }
    if peers.is_empty() {
        return Err(invalid.into());
    }
    Ok((peers, by_node))
}

pub(crate) fn run(home_override: Option<PathBuf>, path: &Path) -> Result<(), String> {
    let source = read_private(path, CONFIG_MAX)
        .ok_or("Owner Hub: select an existing bounded operator-private absolute config file")?;
    let config: Config = toml::from_str(&source).map_err(|_| "Owner Hub: invalid config schema")?;
    if config.version != 1
        || !plain_absolute(&config.auth_socket) || !plain_absolute(&config.netsec_socket)
        || config.auth_socket == config.netsec_socket
        || config.auth_token_file.as_deref().is_some_and(|path| !plain_absolute(path))
        || !plain_absolute(&config.owner_directory) || !plain_absolute(&config.observe_policy_directory)
        || config.owners.is_empty() || config.owners.len() > MAX_OWNERS
    {
        return Err("Owner Hub: invalid bounds, path or listener selection".into());
    }
    let route = HubRoute::select(&config)?;
    let home = crate::home::resolve_home(home_override)?;
    if !plain_absolute(&home) {
        return Err("Owner Hub: the selected Harwness home must be absolute".into());
    }
    let profile = harw_home::profile_dir(&home, &config.profile)
        .map_err(|_| "Owner Hub: invalid profile")?;
    let remote = profile.join(crate::session_serve::STATE_DIR_NAME).join("remote");

    // These pins come from the selected profile and the explicit operator
    // configuration, not from a network handshake/report. A colliding or
    // missing pin never substitutes another identity.
    let hub_node = NodeId::try_from_str(&config.node_id).map_err(|_| "Owner Hub: invalid node ID")?;
    let hub_pin = fixed_pin(&config.hub_key_blake3).ok_or("Owner Hub: invalid Hub node key pin")?;
    let policy_pin = fixed_pin(&config.observe_policy_sha256).ok_or("Owner Hub: invalid policy hash")?;
    let (peers, peer_keys) = read_exact_peers(&remote.join(PEERS_FILE), &hub_node, hub_pin)?;

    let mut links = Vec::with_capacity(config.owners.len());
    let mut process_pins = Vec::with_capacity(config.owners.len());
    let mut destinations = BTreeSet::new();
    let enrolled_keys: BTreeSet<_> = peer_keys.values().map(Vec::as_slice).collect();
    for owner in &config.owners {
        let error = "Owner Hub: invalid, unknown or reused owner proof pin";
        let id = NodeId::try_from_str(&owner.node_id).map_err(|_| error)?;
        let key = peer_keys.get(id.as_str()).ok_or(error)?;
        let process_key = decode_hex(&owner.process_public_key_hex).ok_or(error)?;
        let process_pin = node_key_fingerprint(&process_key).ok_or(error)?;
        if process_pin == hub_pin || enrolled_keys.contains(process_key.as_slice()) {
            return Err(error.into());
        }
        process_pins.push(DestinationProcessKeyPin::new(
            id.as_str(), &owner.instance, owner.epoch, &process_key, key,
        ).map_err(|_| error)?);
        links.push(route.owner_link(owner, NodeIdentity {
            node_id: id,
            public_key: key.clone(),
            key_ref: String::new(), // public-only remote pin; never used for signing
        }, &mut destinations)?);
    }
    let process_pins = OwnerProcessTrustSet::new(process_pins).map_err(|error| error.to_string())?;
    let links = OwnerDirectLinks::new(links).map_err(|error| error.to_string())?;

    // No auto-bootstrap and no synthetic Active owner state. Every grant
    // is checked against both the pinned policy and live profile membership.
    let directory = OwnerDirectory::open(&config.owner_directory)
        .map_err(|_| "Owner Hub: provision the exact owner directory before starting")?;
    let netsec = OwnerEpochClient::new(config.netsec_socket, config.netsec_peer_uid)
        .map_err(|_| "Owner Hub: invalid NetSec socket configuration")?;
    let membership = Arc::new(crate::admin::membership::Registry::open(remote)?);
    let grants = ExplicitDirectObserveGrants::open(
        &config.observe_policy_directory, config.observe_policy_revision, policy_pin, membership,
    ).map_err(|_| "Owner Hub: explicit Observe policy is missing, unsafe or does not match the selected revision and hash")?;
    let ingress = Arc::new(HubTransportIngress::with_pinned_netsec(directory, netsec, grants)
        .with_owner_process_trust(process_pins).with_owner_direct_links(links));

    let token = config.auth_token_file.as_deref().map(BearerToken::read_from_file)
        .transpose().map_err(|_| "Owner Hub: configured AuthHub credential is unavailable")?;
    let auth = AuthHubClient::new_uid_pinned(
        config.auth_socket, config.auth_peer_uid, token, ClientOptions::default(),
    ).map_err(|_| "Owner Hub: invalid AuthHub socket configuration")?;

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()
        .map_err(|_| "Owner Hub: runtime unavailable")?;
    runtime.block_on(async move {
        let key = KeyRef::latest(NODE_IDENTITY_NAMESPACE, hub_node.as_str())
            .map_err(|_| "Owner Hub: invalid AuthHub node identity reference")?;
        let public_key = auth.public_key(&key).await
            .map_err(|_| "Owner Hub: could not read the already-provisioned AuthHub node key")?
            .into_vec();
        if node_key_fingerprint(&public_key) != Some(hub_pin) {
            return Err("Owner Hub: AuthHub node key does not match the independent pin".into());
        }
        let signer = AuthHubNodeSigner::new(Arc::new(HubSign(auth)), &hub_node)
            .and_then(|signer| signer.with_self_check(public_key.clone()))
            .map_err(|_| "Owner Hub: invalid AuthHub node signer")?;
        let local = LocalNode::new(NodeIdentity {
            key_ref: format!("{NODE_IDENTITY_NAMESPACE}/{}", hub_node.as_str()),
            node_id: hub_node,
            public_key,
        }, Arc::new(signer));
        let server = NodeTransportServer::new(
            local.clone(), Arc::new(TranscriptWrappedVerifier::new(peers)), ServerOptions::default(),
        ).map_err(|_| "Owner Hub: could not initialize authenticated node transport")?;
        match route {
            HubRoute::Loopback(address) => {
                let listener = tokio::net::TcpListener::bind(address).await
                    .map_err(|_| "Owner Hub: configured loopback listener is unavailable")?;
                serve_owner_placement_listener(
                    &server, listener, ingress,
                    Some(Arc::new(PinnedOwnerPlacementProofExchange::new(local))),
                    crate::gateway::shutdown_signal(),
                ).await.map_err(|error| format!("Owner Hub listener stopped: {error}"))
            }
            #[cfg(feature = "owner-placement-hub-onion")]
            HubRoute::Onion { locator, server: onion_server, client } => {
                let tor = OnionClient::launch(&client)
                    .map_err(|_| "Owner Hub: could not initialize the configured Onion client")?;
                let exchange = Arc::new(PinnedOwnerPlacementProofExchange::new_onion(
                    local, Arc::new(tor),
                ));
                serve_owner_placement_onion(
                    server, onion_server, &locator, ingress, exchange, crate::gateway::shutdown_signal(),
                ).await.map_err(|_| "Owner Hub: pinned Onion listener unavailable".to_owned())
            }
        }
    })
}
