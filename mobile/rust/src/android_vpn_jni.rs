//! Android TUN descriptor ownership and control callbacks.
#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::BTreeMap;
use std::io;
use std::net::IpAddr;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use harw_onion::{OnionClientConfig, OnionLocator, ProtectedOnionClient, RelaySocketProtector};
use harw_vpn::vpn::{
    DnsLeases, DnsUpstream, FlowKey, PacketIo, Policy, RouteMode, Scope, TunnelEngine,
    dial_with_arti, run_tunnel,
};
use jni::objects::{Global, JByteArray, JObject, JString};
use jni::{EnvUnowned, JValue, JavaVM, Outcome, errors, jni_sig, jni_str};
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

const STARTING: i32 = 1;
const READY: i32 = 2;
const STOPPED: i32 = 3;
const TUN_FAILED: i32 = 4;
const TOR_RETRYING: i32 = 5;
const TOR_FAILED: i32 = 6;

static NEXT_ID: AtomicI64 = AtomicI64::new(1);
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
static HUB: Mutex<Option<HubOwner>> = Mutex::new(None);

struct HubOwner {
    token: i64,
    locator: Option<OnionLocator>,
    client: Weak<ProtectedOnionClient>,
    dns: Arc<Mutex<DnsLeases>>,
}

pub(crate) fn hub_is_held() -> bool {
    HUB.lock().map_or(true, |hub| hub.is_some())
}

/// None means ordinary networking. A held gate without a matching client fails closed.
pub(crate) fn hub_client(
    locator: &OnionLocator,
) -> Result<Option<Arc<ProtectedOnionClient>>, &'static str> {
    let hub = HUB.lock().map_err(|_| "VPN route is unavailable")?;
    let Some(owner) = hub.as_ref() else {
        return Ok(None);
    };
    if owner.locator.as_ref() != Some(locator) {
        return Err("VPN does not own this hub route");
    }
    owner
        .client
        .upgrade()
        .map(Some)
        .ok_or("VPN Tor is not ready")
}

pub(crate) fn hub_progress() -> Option<harw_onion::BootstrapProgress> {
    HUB.lock()
        .ok()?
        .as_ref()?
        .client
        .upgrade()
        .map(|client| client.progress())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_HarwVpnService_nativeSetHubGate(
    _caller: EnvUnowned<'_>,
    _service: JObject<'_>,
    token: i64,
    enabled: jni::sys::jboolean,
    namespace: i32,
) -> jni::sys::jboolean {
    let Ok(mut hub) = HUB.lock() else {
        return false;
    };
    if enabled {
        let Some(dns) = u8::try_from(namespace)
            .ok()
            .and_then(DnsLeases::in_namespace)
        else {
            return false;
        };
        *hub = Some(HubOwner {
            token,
            locator: None,
            client: Weak::new(),
            dns: Arc::new(Mutex::new(dns)),
        });
    } else if hub.as_ref().is_some_and(|owner| owner.token == token) {
        *hub = None;
    }
    true
}

fn attach_hub(token: i64, locator: OnionLocator, client: &Arc<ProtectedOnionClient>) -> bool {
    let Ok(active) = ACTIVE.lock() else {
        return false;
    };
    if !active
        .as_ref()
        .is_some_and(|worker| worker.hub_token == token && worker.stop.is_some())
    {
        return false;
    }

    let Ok(mut hub) = HUB.lock() else {
        return false;
    };
    let Some(owner) = hub.as_mut().filter(|owner| owner.token == token) else {
        return false;
    };
    owner.locator = Some(locator);
    owner.client = Arc::downgrade(client);
    true
}

fn detach_hub(token: i64) {
    if let Ok(mut hub) = HUB.lock()
        && let Some(owner) = hub.as_mut().filter(|owner| owner.token == token)
    {
        owner.locator = None;
        owner.client = Weak::new();
    }
}

struct Active {
    id: i64,
    stop: Option<oneshot::Sender<()>>,
    hub_token: i64,
}

struct Callbacks {
    vm: JavaVM,
    peer: Global<JObject<'static>>,
    id: i64,
}

impl Callbacks {
    fn status(&self, code: i32) {
        let _: errors::Result<()> = self.vm.attach_current_thread_for_scope(|env| {
            env.call_method(
                &self.peer,
                jni_str!("onNativeState"),
                jni_sig!("(JI)V"),
                &[JValue::Long(self.id), JValue::Int(code)],
            )?;
            Ok(())
        });
    }

    fn owner(&self, flow: FlowKey) -> Option<u32> {
        let result: errors::Result<i32> = self.vm.attach_current_thread_for_scope(|env| {
            let source = address_bytes(flow.source.ip());
            let destination = address_bytes(flow.destination.ip());
            let source = env.byte_array_from_slice(source.as_slice())?;
            let destination = env.byte_array_from_slice(destination.as_slice())?;
            env.call_method(
                &self.peer,
                jni_str!("connectionOwnerUid"),
                jni_sig!("([BI[BI)I"),
                &[
                    JValue::Object(source.as_ref()),
                    JValue::Int(i32::from(flow.source.port())),
                    JValue::Object(destination.as_ref()),
                    JValue::Int(i32::from(flow.destination.port())),
                ],
            )?
            .i()
        });
        result.ok().and_then(|uid| uid.try_into().ok())
    }
}

impl RelaySocketProtector for Callbacks {
    fn protect(&self, socket: BorrowedFd<'_>) -> io::Result<()> {
        let result: errors::Result<bool> = self.vm.attach_current_thread_for_scope(|env| {
            env.call_method(
                &self.peer,
                jni_str!("protectRelaySocket"),
                jni_sig!("(I)Z"),
                &[JValue::Int(socket.as_raw_fd())],
            )?
            .z()
        });
        if matches!(result, Ok(true)) {
            Ok(())
        } else {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }
}

fn address_bytes(ip: IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(ip) => ip.octets().to_vec(),
        IpAddr::V6(ip) => ip.octets().to_vec(),
    }
}

struct Tun(AsyncFd<OwnedFd>);

impl Tun {
    fn new(fd: OwnedFd) -> io::Result<Self> {
        let flags = rustix::fs::fcntl_getfl(&fd)?;
        rustix::fs::fcntl_setfl(&fd, flags | rustix::fs::OFlags::NONBLOCK)?;
        // SAFETY: AsyncFd takes exclusive ownership of this duplicate. Its raw
        // descriptor remains open and unchanged until AsyncFd is dropped.
        unsafe { AsyncFd::register(fd) }
            .map(Self)
            .map_err(|error| error.into_parts().1)
    }
}

impl PacketIo for Tun {
    async fn read_packet(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut ready = self.0.readable().await?;
            if let Ok(result) = ready
                .try_io(|fd| rustix::io::read(fd.get_ref(), &mut *bytes).map_err(io::Error::from))
            {
                return result;
            }
        }
    }

    async fn write_packet(&mut self, bytes: &[u8]) -> io::Result<()> {
        loop {
            let mut ready = self.0.writable().await?;
            if let Ok(result) =
                ready.try_io(|fd| rustix::io::write(fd.get_ref(), bytes).map_err(io::Error::from))
            {
                return match result? {
                    written if written == bytes.len() => Ok(()),
                    _ => Err(io::ErrorKind::WriteZero.into()),
                };
            }
        }
    }
}

fn parse_policy(mode: i32, bytes: &[u8]) -> Option<Policy> {
    if bytes.len() > 4096 * 5 || !bytes.len().is_multiple_of(5) {
        return None;
    }
    let scope = match mode {
        0 => Scope::SelectedApps,
        1 => Scope::DeviceWide(RouteMode::OnionOnly),
        2 => Scope::DeviceWide(RouteMode::InternetAndOnion),
        _ => return None,
    };
    let mut rules = BTreeMap::new();
    for entry in bytes.chunks_exact(5) {
        let uid = u32::from_be_bytes(entry[..4].try_into().ok()?);
        let rule = match entry[4] {
            1 => RouteMode::OnionOnly,
            2 => RouteMode::InternetAndOnion,
            _ => return None,
        };
        if rules.insert(uid, rule).is_some() {
            return None;
        }
    }
    Some(Policy::new(scope, rules))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_HarwVpnService_nativeStart<'local>(
    mut caller: EnvUnowned<'local>,
    service: JObject<'local>,
    descriptor: i32,
    directory: JString<'local>,
    mode: i32,
    rules: JByteArray<'local>,
    seed: i64,
    hub_token: i64,
    locator: JString<'local>,
    dns_endpoint: JString<'local>,
) -> i64 {
    let result = caller.with_env(|env| -> errors::Result<i64> {
        if descriptor < 0 || rules.len(env)? > 4096 * 5 {
            return Ok(0);
        }
        let rules = env.convert_byte_array(rules)?;
        let Some(policy) = parse_policy(mode, &rules) else {
            return Ok(0);
        };
        let directory = PathBuf::from(directory.try_to_string(env)?);
        let Ok(locator) = locator.try_to_string(env)?.parse::<OnionLocator>() else {
            return Ok(0);
        };
        let Ok(upstream) = DnsUpstream::parse(&dns_endpoint.try_to_string(env)?) else {
            return Ok(0);
        };
        let vm = env.get_java_vm()?;
        let peer = env.new_global_ref(service)?;
        // SAFETY: the private service method borrows an open ParcelFileDescriptor
        // for this call. Duplicate it before returning and never close its copy.
        let borrowed = unsafe { BorrowedFd::borrow_raw(descriptor) };
        let Ok(fd) = rustix::io::fcntl_dupfd_cloexec(borrowed, 0) else {
            return Ok(0);
        };
        let Ok(mut active) = ACTIVE.lock() else {
            return Ok(0);
        };
        if active.is_some() {
            return Ok(0);
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (stop, stopped) = oneshot::channel();
        let callbacks = Arc::new(Callbacks { vm, peer, id });
        *active = Some(Active {
            id,
            stop: Some(stop),
            hub_token,
        });

        let launched = std::thread::Builder::new()
            .name("harw-vpn".into())
            .spawn(move || {
                let status = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run(
                        fd,
                        directory,
                        policy,
                        seed as u64,
                        stopped,
                        callbacks.clone(),
                        hub_token,
                        locator,
                        upstream,
                    )),
                    Err(_) => TUN_FAILED,
                };
                detach_hub(hub_token);
                if let Ok(mut active) = ACTIVE.lock() {
                    *active = None;
                }
                callbacks.status(status);
            });
        if launched.is_err() {
            *active = None;
            return Ok(0);
        }
        Ok(id)
    });
    match result.into_outcome() {
        Outcome::Ok(id) => id,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_HarwVpnService_nativeStop(
    _caller: EnvUnowned<'_>,
    _service: JObject<'_>,
    id: i64,
) {
    if let Ok(mut active) = ACTIVE.lock()
        && let Some(active) = active.as_mut().filter(|active| active.id == id)
        && let Some(stop) = active.stop.take()
    {
        detach_hub(active.hub_token);
        let _ = stop.send(());
    }
}

async fn run(
    fd: OwnedFd,
    directory: PathBuf,
    policy: Policy,
    seed: u64,
    stopped: oneshot::Receiver<()>,
    callbacks: Arc<Callbacks>,
    hub_token: i64,
    locator: OnionLocator,
    upstream: Option<DnsUpstream>,
) -> i32 {
    let Ok(tun) = Tun::new(fd) else {
        return TUN_FAILED;
    };
    let config = OnionClientConfig::new(directory.join("state"), directory.join("cache"));
    let Ok(client) = ProtectedOnionClient::new(&config, callbacks.clone()) else {
        return TOR_FAILED;
    };
    let client = Arc::new(client);
    callbacks.status(STARTING);
    let boot_client = client.clone();
    let boot_callbacks = callbacks.clone();
    let bootstrap = tokio::spawn(async move {
        let mut retry = Duration::from_secs(1);
        loop {
            if boot_client.bootstrap().await.is_ok() {
                if attach_hub(hub_token, locator, &boot_client) {
                    boot_callbacks.status(READY);
                }
                break;
            }
            boot_callbacks.status(TOR_RETRYING);
            tokio::time::sleep(retry).await;
            retry = (retry * 2).min(Duration::from_secs(30));
        }
    });
    let dns = HUB.lock().ok().and_then(|hub| {
        hub.as_ref()
            .filter(|owner| owner.token == hub_token)
            .map(|owner| owner.dns.clone())
    });
    let Some(dns) = dns else {
        bootstrap.abort();
        return TOR_FAILED;
    };
    let engine = TunnelEngine::with_dns(policy, Instant::now(), seed, dns);
    let upstream = upstream.map(Arc::new);
    let result = run_tunnel(
        tun,
        engine,
        |flow| callbacks.owner(flow),
        move |destination, timeout| {
            let client = client.clone();
            let upstream = upstream.clone();
            async move { dial_with_arti(&client, upstream.as_deref(), destination, timeout).await }
        },
        async {
            let _ = stopped.await;
        },
    )
    .await;
    bootstrap.abort();
    match result {
        Ok(()) => STOPPED,
        Err(_) => TUN_FAILED,
    }
}
