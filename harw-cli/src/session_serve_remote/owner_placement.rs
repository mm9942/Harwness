//! Explicit independent Hub pin and owner process credential composition.
//! The operator provisions a separate key for the exact owner incarnation;
//! startup cannot mint a key, grant, or alternate route.

use std::{
    collections::BTreeSet,
    fs::File,
    future::Future,
    io::{ErrorKind, Read as _},
    net::SocketAddr,
    num::NonZeroU64,
    os::{fd::AsFd as _, unix::fs::MetadataExt as _},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::Duration,
};

use harw_cloud_gateway::{
    owner_ingress::HubIngressError,
    owner_placement::OwnerPlacementLookupClient,
    owner_process_proof::{DestinationProcessKeyPin, OwnerProcessCustody},
    owner_proof_exchange::{PinnedReservedObserveSigner, ReservedObserveSigner, owner_observe_proof_response},
    route_admission::OwnerFence,
};
use harw_node_listener::{OwnerObserveProofHandler, OwnerProofIngress};
use harw_node_transport::{
    LocalNode, ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, NodeBody, NodeIdentity,
    OwnerProcessChannelBinding, node_key_fingerprint,
};
use harw_protocol::{PortError, PortFuture, owner_placement::OwnerPlacementLookupId};
use harw_session_host::{CurrentOwnerPlacement, DirectOwnerPlacementAuthority, OwnerProcessPin, SessionHost};
use harw_types::{NodeId, SessionId, TenantId};
use http::{Request, Response};
use hyper::body::Incoming;
use tokio::sync::{Semaphore, watch};
use tokio::task::{AbortHandle, JoinSet};
use zeroize::Zeroizing;

use super::PeerPolicyGate;

const PIN_FILE: &str = "owner-placement-hub.toml";
const MAX_PIN_BYTES: u64 = 16 * 1024;
const MAX_ASSIGNMENTS: usize = 4;
const REPORT_PERIOD: Duration = Duration::from_secs(10);
const PROCESS_CREDENTIAL: &str = "harw-owner-observe-ml-dsa-65";
static PIN_READS: Semaphore = Semaphore::const_new(8);

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HubPinFile {
    onion_locator: Option<String>,
    loopback_address: Option<SocketAddr>,
    node_id: String,
    public_key_hex: String,
    owner_epoch: NonZeroU64,
    process_public_key_hex: String,
    #[serde(default)]
    report_assignments: Vec<ReportAssignmentConfig>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportAssignmentConfig {
    tenant: String,
    session: String,
    generation: NonZeroU64,
}

#[derive(Clone)]
struct ReportAssignment {
    fence: OwnerFence,
    tenant: TenantId,
    session: SessionId,
}

fn configured_assignments(
    rows: Vec<ReportAssignmentConfig>,
    node: &NodeId,
    instance: &str,
    epoch: NonZeroU64,
) -> Result<Vec<ReportAssignment>, String> {
    let invalid = || format!("invalid exact owner report assignments in {PIN_FILE}");
    if rows.len() > MAX_ASSIGNMENTS {
        return Err(invalid());
    }
    let mut seen = BTreeSet::new();
    rows.into_iter().map(|row| {
        let valid = |value: &str| !value.is_empty() && value.len() <= 255 && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
        });
        if !valid(&row.tenant) || !valid(&row.session)
            || !seen.insert(row.session.clone())
        {
            return Err(invalid());
        }
        let tenant: TenantId = row.tenant.clone().try_into().map_err(|_| invalid())?;
        let session: SessionId = row.session.clone().try_into().map_err(|_| invalid())?;
        Ok(ReportAssignment {
            fence: OwnerFence {
                tenant: row.tenant,
                session_id: row.session,
                node_id: node.as_str().to_owned(),
                host_instance: instance.to_owned(),
                generation: row.generation,
                owner_epoch: epoch.get(),
            },
            tenant,
            session,
        })
    }).collect()
}

fn unavailable() -> PortError {
    PortError::Denied("current owner placement unavailable".into())
}

struct HubPinGate {
    path: PathBuf,
    directory: File,
    digest: [u8; 32],
    withdrawn: AtomicBool,
}

impl HubPinGate {
    fn open_directory(path: &Path) -> Result<File, PortError> {
        if !path.is_absolute() {
            return Err(unavailable());
        }
        let directory = File::from(harw_fsutil::open_dir_nofollow(path).map_err(|_| unavailable())?);
        harw_fsutil::ensure_owned_directory(&directory).map_err(|_| unavailable())?;
        if directory.metadata().map_err(|_| unavailable())?.mode() & 0o077 != 0 {
            return Err(unavailable());
        }
        Ok(directory)
    }

    fn read(directory: &File) -> Result<String, PortError> {
        let file = harw_fsutil::open_beneath(
            directory.as_fd(), Path::new(PIN_FILE), harw_fsutil::OpenMode::read_only(),
        ).map_err(|_| unavailable())?;
        harw_fsutil::ensure_private_regular(&file).map_err(|_| unavailable())?;
        let metadata = file.metadata().map_err(|_| unavailable())?;
        if metadata.nlink() != 1 || metadata.len() > MAX_PIN_BYTES {
            return Err(unavailable());
        }
        let mut text = String::new();
        (&file).take(MAX_PIN_BYTES + 1).read_to_string(&mut text).map_err(|_| unavailable())?;
        if text.len() as u64 > MAX_PIN_BYTES
            || !harw_fsutil::same_file_at(directory.as_fd(), Path::new(PIN_FILE), &file)
                .map_err(|_| unavailable())?
        {
            return Err(unavailable());
        }
        Ok(text)
    }

    fn current(&self) -> Result<(), PortError> {
        if self.withdrawn.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let result = (|| {
            let directory = Self::open_directory(&self.path)?;
            let a = self.directory.metadata().map_err(|_| unavailable())?;
            let b = directory.metadata().map_err(|_| unavailable())?;
            if (a.dev(), a.ino()) != (b.dev(), b.ino())
                || *blake3::hash(Self::read(&directory)?.as_bytes()).as_bytes() != self.digest
            {
                return Err(unavailable());
            }
            Ok(())
        })();
        if result.is_err() {
            self.withdrawn.store(true, Ordering::Release);
        }
        if self.withdrawn.load(Ordering::Acquire) {
            Err(unavailable())
        } else {
            result
        }
    }
}

pub(super) struct ConfiguredPlacementAuthority {
    gate: Arc<ProofGate>,
    client: Arc<OwnerPlacementLookupClient>,
    owner_epoch: NonZeroU64,
    ingress: OwnerProofIngress,
    assignments: Vec<ReportAssignment>,
}

fn decode_public_key(text: &str) -> Option<Vec<u8>> {
    if text.len() != ML_DSA_65_PUBLIC_KEY_LEN * 2 {
        return None;
    }
    data_encoding::HEXLOWER.decode(text.as_bytes()).ok()
}

impl ConfiguredPlacementAuthority {
    pub(super) fn open(
        directory: &Path,
        local: LocalNode,
        peer_policy: Arc<PeerPolicyGate>,
        instance: &str,
        node_bind: SocketAddr,
        onion_ingress: bool,
    ) -> Result<(Self, OwnerProcessCustody), String> {
        let error = || format!("direct owner placement requires a valid private {PIN_FILE}");
        let dir = HubPinGate::open_directory(directory).map_err(|_| error())?;
        let snapshot = HubPinGate::read(&dir).map_err(|_| error())?;
        let pin: HubPinFile = toml::from_str(&snapshot).map_err(|_| error())?;
        let hub_id = NodeId::try_from_str(&pin.node_id).map_err(|_| error())?;
        let public_key = decode_public_key(&pin.public_key_hex).ok_or_else(error)?;
        let hub_key = node_key_fingerprint(&public_key).ok_or_else(error)?;
        let process_public_key = decode_public_key(&pin.process_public_key_hex).ok_or_else(error)?;
        // The process credential is not the authenticated Hub or the owner
        // node key, even when the wrong credential is valid ML-DSA-65.
        if process_public_key == public_key || process_public_key == local.identity.public_key {
            return Err(error());
        }
        let hub = NodeIdentity {
            node_id: hub_id.clone(),
            public_key,
            key_ref: "public-owner-placement-pin".into(),
        };
        let gate = Arc::new(ProofGate {
            pin: HubPinGate {
                path: directory.to_owned(),
                directory: dir,
                digest: *blake3::hash(snapshot.as_bytes()).as_bytes(),
                withdrawn: AtomicBool::new(false),
            },
            peer_policy,
            hub: hub_id,
            hub_key,
        });
        gate.current().map_err(|_| "the independent Hub pin is not current in both owner trust files")?;
        let process_pin = DestinationProcessKeyPin::new(
            local.node_id().as_str(), instance, pin.owner_epoch,
            &process_public_key, &local.identity.public_key,
        ).map_err(|_| "the independent owner process key pin is invalid")?;
        let assignments = configured_assignments(pin.report_assignments, local.node_id(), instance, pin.owner_epoch)?;
        // Reject a missing or mismatched credential before consuming a new
        // durable host epoch. The selected key is not usable until composed
        // against the actually opened Host below.
        let seed = load_process_credential()?;
        let custody = OwnerProcessCustody::from_process_seed(process_pin, &seed, gate.hub.as_str())
            .map_err(|_| "the dedicated owner process credential does not match the process pin")?;
        drop(seed);
        let (client, ingress) = match (pin.onion_locator, pin.loopback_address) {
            (Some(locator), None) if onion_ingress => (
                prepare_onion_lookup(directory, &locator, local, hub)?,
                OwnerProofIngress::EstablishedStreamOnly,
            ),
            (Some(_), None) => return Err("Onion owner placement needs [session_listener] onion = true".into()),
            (None, Some(address)) if node_bind.ip().is_loopback() => (
                OwnerPlacementLookupClient::new_loopback(address, local, hub).map_err(|_| error())?,
                OwnerProofIngress::LoopbackLabOnly,
            ),
            (None, Some(_)) => return Err("loopback owner placement requires a loopback owner listener".into()),
            _ => return Err(format!("exactly one onion_locator or loopback_address is required in {PIN_FILE}")),
        };
        gate.current().map_err(|_| "current independent Hub trust was withdrawn during owner setup")?;
        Ok((Self {
            gate,
            client: Arc::new(client),
            owner_epoch: pin.owner_epoch,
            ingress,
            assignments,
        }, custody))
    }

    pub(super) fn hub_node(&self) -> &NodeId {
        &self.gate.hub
    }

    pub(super) fn ingress(&self) -> OwnerProofIngress {
        self.ingress
    }

    pub(super) fn reporter(&self, pin: Arc<OwnerProcessPin>) -> Option<ConfiguredOwnerReporter> {
        (!self.assignments.is_empty()).then(|| ConfiguredOwnerReporter {
            gate: Arc::clone(&self.gate),
            client: Arc::clone(&self.client),
            pin,
            assignments: self.assignments.clone(),
        })
    }

    pub(super) fn compose_proof(
        &self,
        host: &SessionHost,
        custody: OwnerProcessCustody,
    ) -> Result<Arc<dyn OwnerObserveProofHandler>, String> {
        if self.owner_epoch.get() != host.epoch() {
            return Err("the configured owner epoch does not match the newly opened host; provision fresh trust for its next epoch".into());
        }
        self.gate.current().map_err(|_| "current independent Hub trust was withdrawn during owner setup")?;
        Ok(Arc::new(ConfiguredOwnerProof {
            gate: Arc::clone(&self.gate),
            signer: PinnedReservedObserveSigner::new(host.clone(), custody, Arc::clone(&self.client)),
        }))
    }
}

#[cfg(feature = "onion")]
fn prepare_onion_lookup(
    directory: &Path,
    locator: &str,
    local: LocalNode,
    hub: NodeIdentity,
) -> Result<OwnerPlacementLookupClient, String> {
    use harw_onion::{OnionClient, OnionClientConfig, OnionLocator};

    let locator: OnionLocator = locator.parse()
        .map_err(|_| "owner placement needs an exact v3 onion locator with virtual port")?;
    let root = directory.join("owner-placement-tor-client");
    let state = root.join("state");
    let cache = root.join("cache");
    for dir in [&root, &state, &cache] {
        crate::session_serve::private_dir(dir)?;
        HubPinGate::open_directory(dir).map_err(|_| "owner placement Tor state is not private")?;
    }
    // One isolated client for the lifetime of the configured authority,
    // warm it independently of any one-use reservation or incoming request.
    let client = OnionClient::launch(&OnionClientConfig::new(state, cache))
        .map_err(|_| "owner placement Arti client is unavailable")?;
    OwnerPlacementLookupClient::new_onion(Arc::new(client), locator, local, hub)
        .map_err(|_| "independent owner placement Hub pin is invalid".into())
}

#[cfg(not(feature = "onion"))]
fn prepare_onion_lookup(
    _directory: &Path,
    _locator: &str,
    _local: LocalNode,
    _hub: NodeIdentity,
) -> Result<OwnerPlacementLookupClient, String> {
    Err("pinned Onion owner placement requires a build with the onion feature".into())
}

/// Fixed systemd credential supplied to this process only. No ad hoc
/// key-file path, command-line secret, generated key or node-key fallback.
fn load_process_credential() -> Result<Zeroizing<[u8; 32]>, String> {
    let invalid = || format!("direct owner placement requires systemd credential {PROCESS_CREDENTIAL}");
    let path = std::env::var_os("CREDENTIALS_DIRECTORY").ok_or_else(invalid)?;
    let directory = HubPinGate::open_directory(Path::new(&path)).map_err(|_| invalid())?;
    let name = Path::new(PROCESS_CREDENTIAL);
    let file = harw_fsutil::open_beneath(
        directory.as_fd(), name, harw_fsutil::OpenMode::read_only(),
    ).map_err(|_| invalid())?;
    harw_fsutil::ensure_private_regular(&file).map_err(|_| invalid())?;
    let metadata = file.metadata().map_err(|_| invalid())?;
    if metadata.len() != 32 || metadata.nlink() != 1 || metadata.mode() & 0o222 != 0 {
        return Err(invalid());
    }
    let mut source = &file;
    let mut bytes = Zeroizing::new([0; 32]);
    source.read_exact(&mut bytes[..]).map_err(|_| invalid())?;
    let mut extra = Zeroizing::new([0u8; 1]);
    loop {
        match source.read(&mut extra[..]) {
            Ok(0) => break,
            Err(error) if error.kind() == ErrorKind::Interrupted => {},
            _ => return Err(invalid()),
        }
    }
    if !harw_fsutil::same_file_at(directory.as_fd(), name, &file).map_err(|_| invalid())? {
        return Err(invalid());
    }
    Ok(bytes)
}

struct ProofGate {
    pin: HubPinGate,
    peer_policy: Arc<PeerPolicyGate>,
    hub: NodeId,
    hub_key: [u8; 32],
}

impl ProofGate {
    fn current(&self) -> Result<(), PortError> {
        self.pin.current()?;
        if self.peer_policy.expected.is_none()
            || self.peer_policy.fingerprints.get(self.hub.as_str()) != Some(&self.hub_key)
            || !self.peer_policy.current()
        {
            return Err(unavailable());
        }
        Ok(())
    }
}

async fn pin_read<T: Send + 'static>(
    read: impl FnOnce() -> Result<T, PortError> + Send + 'static,
) -> Result<T, PortError> {
    // Owned slot follows the file read even if the caller drops its future.
    let permit = PIN_READS.try_acquire().map_err(|_| unavailable())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        read()
    }).await.map_err(|_| unavailable())?
}

pub(super) struct ConfiguredOwnerReporter {
    gate: Arc<ProofGate>,
    client: Arc<OwnerPlacementLookupClient>,
    pin: Arc<OwnerProcessPin>,
    assignments: Vec<ReportAssignment>,
}

impl ConfiguredOwnerReporter {
    /// The caller starts this only after the selected owner ingress is up.
    /// No Report ever allocates a placement or modifies the selected host.
    pub(super) async fn run(self, listener: AbortHandle, mut stop: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(REPORT_PERIOD);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = stop.wait_for(|stop| *stop) => return,
                _ = tick.tick() => {}
            }
            if listener.is_finished() {
                return;
            }
            let mut work = JoinSet::new();
            for assignment in self.assignments.iter().cloned() {
                let client = Arc::clone(&self.client);
                let gate = Arc::clone(&self.gate);
                let pin = Arc::clone(&self.pin);
                let listener = listener.clone();
                work.spawn(async move {
                    let fence = assignment.fence.clone();
                    let check = || async {
                        let gate = Arc::clone(&gate);
                        let pin = Arc::clone(&pin);
                        let entry = assignment.clone();
                        let listener = listener.clone();
                        pin_read(move || {
                            if listener.is_finished() {
                                return Err(unavailable());
                            }
                            gate.current()?;
                            let epoch = NonZeroU64::new(entry.fence.owner_epoch).ok_or_else(unavailable)?;
                            pin.validate_publication(&entry.fence.node_id, &entry.fence.host_instance,
                                epoch, &entry.session, &entry.tenant).map_err(|_| unavailable())
                        }).await.is_ok()
                    };
                    if check().await {
                        // A second independent point-in-time check takes
                        // place after the pin-authenticated dial, before POST.
                        let _ = client.publish_owner_report_once(&fence, check).await;
                    }
                });
            }
            while !work.is_empty() {
                tokio::select! {
                    biased;
                    _ = stop.wait_for(|stop| *stop) => return,
                    _ = work.join_next() => {}
                }
            }
        }
    }
}

struct ConfiguredOwnerProof {
    gate: Arc<ProofGate>,
    signer: PinnedReservedObserveSigner,
}

impl OwnerObserveProofHandler for ConfiguredOwnerProof {
    fn handle(
        &self,
        request: Request<Incoming>,
    ) -> Pin<Box<dyn Future<Output = Response<NodeBody>> + Send + '_>> {
        Box::pin(owner_observe_proof_response(request, self.gate.hub.as_str(), self.gate.hub_key, self))
    }
}

impl ReservedObserveSigner for ConfiguredOwnerProof {
    fn sign<'a>(
        &'a self,
        channel: &'a OwnerProcessChannelBinding,
        reservation: [u8; 32],
        presign: [u8; 32],
        transcript: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<[u8; ML_DSA_65_SIGNATURE_LEN], HubIngressError>> + Send + 'a>> {
        Box::pin(async move {
            let check = || {
                let gate = Arc::clone(&self.gate);
                pin_read(move || gate.current())
            };
            check().await.map_err(|_| HubIngressError::Forbidden)?;
            let signature = self.signer.sign(channel, reservation, presign, transcript).await?;
            check().await.map_err(|_| HubIngressError::Forbidden)?;
            Ok(signature)
        })
    }
}

impl DirectOwnerPlacementAuthority for ConfiguredPlacementAuthority {
    fn lookup(&self, id: OwnerPlacementLookupId) -> PortFuture<'_, CurrentOwnerPlacement> {
        Box::pin(async move {
            let check = || {
                let gate = Arc::clone(&self.gate);
                pin_read(move || gate.current())
            };
            check().await?;
            let answer = self.client.lookup(id).await?;
            check().await?;
            Ok(answer)
        })
    }
}
